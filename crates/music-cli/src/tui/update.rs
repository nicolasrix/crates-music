//! The reducer: `update(&mut App, Msg) -> Vec<Effect>`. Pure over app state
//! plus (deliberately) direct calls into the `Player` handle — those are
//! fire-and-forget channel sends, safe and instant, and threading them
//! through effects would only add latency to keypresses.

use super::msg::{Effect, Msg, StationError};
use super::signal::{
    self, MAX_EVENT_ATTEMPTS, PendingEvent, ScrobbleDecision, TrackSignal,
};
use super::state::{
    ALBUM_KINDS, App, LibraryPane, Loadable, Overlay, Rating, SearchBucket, Section, to_queued,
};

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;

const RECOMMEND_N: usize = 20;
const STATION_N: usize = 30;
const ALBUM_PAGE: u32 = 100;
/// Event-outbox flush cadence in ticks (~5 s at the 250 ms tick).
const FLUSH_EVERY_TICKS: u64 = 20;

/// How the queue moved off the current track: a user gesture (skip signal)
/// or the track draining / failing on its own (no signal).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Advance {
    Manual,
    Natural,
}

// A flat message dispatcher, like app.rs's command match — the length is the
// enum's, not the logic's; per-arm work already lives in helper fns.
#[allow(clippy::too_many_lines)]
pub(crate) fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Tick => {
            app.tick += 1;
            if app.status.as_ref().is_some_and(|s| app.tick >= s.expires_at) {
                app.status = None;
            }
            signal_tick(app)
        }
        Msg::Quit => {
            app.should_quit = true;
            vec![]
        }
        Msg::ToggleHelp => {
            app.overlay = if app.overlay == Overlay::Help {
                Overlay::None
            } else {
                Overlay::Help
            };
            vec![]
        }
        Msg::Back => back(app),
        Msg::GoSection(s) => go_section(app, s),
        Msg::NextSection => {
            let next = (app.section.index() + 1) % Section::ALL.len();
            go_section(app, Section::ALL[next])
        }
        Msg::PrevSection => {
            let len = Section::ALL.len();
            let prev = (app.section.index() + len - 1) % len;
            go_section(app, Section::ALL[prev])
        }
        Msg::NavUp => nav(app, -1),
        Msg::NavDown => nav(app, 1),
        Msg::NavTop => nav_to(app, NavTarget::Top),
        Msg::NavBottom => nav_to(app, NavTarget::Bottom),
        Msg::NavHalfPageDown => nav(app, 10),
        Msg::NavHalfPageUp => nav(app, -10),
        Msg::CycleKindPrev => cycle_kind(app, -1),
        Msg::CycleKindNext => cycle_kind(app, 1),
        Msg::Activate => activate(app),
        Msg::Enqueue => enqueue_selected(app),
        Msg::FocusSearch => {
            app.section = Section::Search;
            app.search.focused = true;
            vec![]
        }
        Msg::FocusInput => {
            match app.section {
                Section::Search => app.search.focused = true,
                Section::Stations => app.stations.focused = true,
                _ => {}
            }
            vec![]
        }
        Msg::Input(im) => {
            match app.section {
                Section::Search if app.search.focused => app.search.input.apply(&im),
                Section::Stations if app.stations.focused => app.stations.input.apply(&im),
                _ => {}
            }
            vec![]
        }
        Msg::SubmitInput => submit_input(app),
        Msg::TransportToggle => transport_toggle(app),
        Msg::TransportNext => next_track(app, Advance::Manual),
        Msg::TransportPrev => prev_track(app),
        Msg::SeekBy(delta) => {
            if let Some(p) = &app.player {
                p.seek_by(delta);
            }
            vec![]
        }
        Msg::VolumeBy(delta) => {
            if let Some(p) = &app.player {
                p.set_volume(app.playback.volume + delta);
            }
            vec![]
        }
        Msg::Rate(verdict) => rate_selected(app, verdict),
        Msg::RecommendFromNowPlaying => recommend_from_now_playing(app),
        Msg::QueueRemoveSelected => queue_remove_selected(app),
        Msg::QueueClear => {
            note_abandonment(app);
            app.queue.clear();
            app.prefetched = None;
            app.pending_load = None;
            app.queue_table.select(None);
            app.player_stop();
            vec![]
        }
        Msg::Player(ev) => player_event(app, ev),

        // ── effect completions ────────────────────────────────────────
        Msg::AlbumsLoaded { generation, result } => {
            if generation == app.library.generation {
                app.library.albums = loadable_from(result);
                select_first(&mut app.library.albums_table, loaded_len(&app.library.albums));
            }
            vec![]
        }
        Msg::AlbumOpened { id, result } => {
            if app.library.open_target.as_deref() == Some(id.as_str()) {
                app.library.open_album = loadable_from(result);
                let len = app
                    .library
                    .open_album
                    .ready()
                    .map_or(0, |a| a.tracks.len());
                select_first(&mut app.library.tracks_table, len);
            }
            vec![]
        }
        Msg::AlbumTracksForEnqueue { result } => {
            match result {
                Ok(album) => {
                    let n = album.tracks.len();
                    app.queue.enqueue(album.tracks.iter().map(to_queued).collect());
                    app.set_status(
                        format!("queued {n} track(s) from {}", album.album.name),
                        false,
                    );
                }
                Err(e) => app.set_status(format!("enqueue failed: {e}"), true),
            }
            vec![]
        }
        Msg::SearchDone { generation, result } => {
            if generation == app.search.generation {
                app.search.results = loadable_from(result);
                let lens = search_bucket_lens(app);
                for (table, len) in app.search.tables.iter_mut().zip(lens) {
                    select_first(table, len);
                }
            }
            vec![]
        }
        Msg::StationDone { generation, result } => {
            if generation == app.stations.generation {
                app.stations.results = match result {
                    Ok(tracks) => Loadable::Ready(tracks),
                    Err(StationError::Unavailable) => Loadable::Failed(
                        "station unavailable — the gateway recommender is warming up \
                         or the embedder is offline"
                            .to_owned(),
                    ),
                    Err(StationError::Other(e)) => Loadable::Failed(e),
                };
                select_first(
                    &mut app.stations.table,
                    loaded_len(&app.stations.results),
                );
            }
            vec![]
        }
        Msg::RecommendDone { result } => {
            match result {
                Ok(tracks) => {
                    let n = tracks.len();
                    app.queue.enqueue(tracks.iter().map(to_queued).collect());
                    app.set_status(format!("queued {n} similar track(s)"), false);
                }
                Err(StationError::Unavailable) => app.set_status(
                    "recommendations unavailable — recommender warming up or seed not embedded",
                    true,
                ),
                Err(StationError::Other(e)) => {
                    app.set_status(format!("recommend failed: {e}"), true);
                }
            }
            vec![]
        }
        Msg::LikedLoaded { result } => {
            app.liked.entries = loadable_from(result);
            // Seed the optimistic ratings map from the server's truth.
            if let Loadable::Ready(entries) = &app.liked.entries {
                for e in entries {
                    app.ratings.insert(e.id.clone(), e.rating);
                }
            }
            select_first(&mut app.liked.table, loaded_len(&app.liked.entries));
            vec![]
        }
        Msg::RatingSet {
            id,
            previous,
            result,
        } => {
            if let Err(e) = result {
                // Roll the optimistic update back.
                match previous {
                    Some(r) => {
                        app.ratings.insert(id, r);
                    }
                    None => {
                        app.ratings.remove(&id);
                    }
                }
                app.set_status(format!("rating failed: {e}"), true);
            }
            vec![]
        }
        Msg::AudioReady {
            queue_index,
            track_id,
            bytes,
        } => {
            // Stale if the queue moved on while the fetch was in flight.
            let current = app.queue.current();
            if app.queue.current_index() != Some(queue_index)
                || current.is_none_or(|t| t.id != track_id)
            {
                return vec![];
            }
            let duration = current.and_then(|t| t.duration);
            app.player_load(bytes, track_id, duration);
            app.pending_load = None;
            prefetch_next(app)
        }
        Msg::AudioFailed {
            queue_index,
            track_id,
            error,
        } => {
            if app.queue.current_index() != Some(queue_index)
                || app.queue.current().is_none_or(|t| t.id != track_id)
            {
                return vec![];
            }
            app.pending_load = None;
            app.set_status(format!("skipping {track_id}: {error}"), true);
            // Not a user abandonment — the track never played.
            next_track(app, Advance::Natural)
        }
        Msg::PrefetchReady { track_id, bytes } => {
            // Only keep it if that track is still next up.
            if app.queue.next_up().is_some_and(|t| t.id == track_id) {
                app.prefetched = Some((track_id, bytes));
            }
            vec![]
        }
        Msg::EventsFlushed { events, result } => {
            app.events_inflight = false;
            if let Err(e) = result {
                requeue_failed_events(app, events, &e);
            }
            vec![]
        }
    }
}

// ── listening signal ────────────────────────────────────────────────────

/// Per-tick signal work: keep the per-track emission state in step with
/// what the player reports, fire due scrobbles, and flush the event outbox
/// on its cadence.
fn signal_tick(app: &mut App) -> Vec<Effect> {
    let mut effects = Vec::new();

    if let Some(id) = app.playback.track_id.clone() {
        if app.signal.as_ref().is_none_or(|s| s.track_id != id) {
            app.signal = Some(TrackSignal::new(id.clone()));
        }
        let sig = app.signal.as_mut().expect("just ensured above");
        match signal::evaluate_scrobble(app.playback.duration, app.playback.position, sig) {
            ScrobbleDecision::NowPlaying => {
                sig.now_playing_sent = true;
                effects.push(Effect::Scrobble {
                    track_id: id,
                    submission: false,
                });
            }
            ScrobbleDecision::Submission => {
                sig.submission_sent = true;
                effects.push(Effect::Scrobble {
                    track_id: id,
                    submission: true,
                });
            }
            ScrobbleDecision::None => {}
        }
    }

    if !app.events_outbox.is_empty()
        && !app.events_inflight
        && app.tick.is_multiple_of(FLUSH_EVERY_TICKS)
    {
        app.events_inflight = true;
        effects.push(Effect::FlushEvents {
            events: std::mem::take(&mut app.events_outbox),
        });
    }
    effects
}

/// The current track is being *manually* abandoned (next/prev, activating
/// another track, removing or clearing it). Record at most one skip verdict
/// per load — the gate itself (too short / never started) lives in
/// [`signal::evaluate_skip`]. Natural end-of-track never comes through here.
fn note_abandonment(app: &mut App) {
    if !app.events_enabled {
        return;
    }
    let Some(id) = app.playback.track_id.clone() else {
        return;
    };
    if app
        .signal
        .as_ref()
        .is_some_and(|s| s.track_id == id && s.abandoned)
    {
        return;
    }
    if let Some(played_ms) = signal::evaluate_skip(app.playback.duration, app.playback.position) {
        app.events_outbox.push(PendingEvent::skip(id.clone(), played_ms));
    }
    // Mark the verdict taken even when gated out — one decision per load.
    match app.signal.as_mut() {
        Some(s) if s.track_id == id => s.abandoned = true,
        _ => {
            let mut s = TrackSignal::new(id);
            s.abandoned = true;
            app.signal = Some(s);
        }
    }
}

/// Put failed events back at the front of the outbox (order preserved),
/// dropping any that exhausted their attempts.
fn requeue_failed_events(app: &mut App, events: Vec<PendingEvent>, error: &str) {
    tracing::debug!(count = events.len(), error, "event flush failed");
    let mut retained: Vec<PendingEvent> = events
        .into_iter()
        .filter_map(|mut ev| {
            ev.attempts += 1;
            (ev.attempts < MAX_EVENT_ATTEMPTS).then_some(ev)
        })
        .collect();
    retained.append(&mut app.events_outbox);
    app.events_outbox = retained;
}

// ── navigation ─────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum NavTarget {
    Top,
    Bottom,
}

/// The (row count, table state) pair the cursor keys act on right now.
fn focused_list(app: &mut App) -> (usize, &mut ratatui::widgets::TableState) {
    match app.section {
        Section::Library => match app.library.pane {
            LibraryPane::Albums => (
                loaded_len(&app.library.albums),
                &mut app.library.albums_table,
            ),
            LibraryPane::AlbumDetail => (
                app.library.open_album.ready().map_or(0, |a| a.tracks.len()),
                &mut app.library.tracks_table,
            ),
        },
        Section::Search => {
            let lens = search_bucket_lens(app);
            let idx = app.search.bucket % 3;
            (lens[idx], &mut app.search.tables[idx])
        }
        Section::Queue => (app.queue.len(), &mut app.queue_table),
        Section::Stations => (
            loaded_len(&app.stations.results),
            &mut app.stations.table,
        ),
        Section::Liked => (loaded_len(&app.liked.entries), &mut app.liked.table),
    }
}

fn search_bucket_lens(app: &App) -> [usize; 3] {
    app.search.results.ready().map_or([0; 3], |r| {
        [r.tracks.len(), r.albums.len(), r.artists.len()]
    })
}

fn loaded_len<T>(l: &Loadable<Vec<T>>) -> usize {
    l.ready().map_or(0, Vec::len)
}

fn nav(app: &mut App, delta: i64) -> Vec<Effect> {
    let (len, table) = focused_list(app);
    if len == 0 {
        table.select(None);
        return vec![];
    }
    let cur = table.selected().unwrap_or(0);
    let max = i64::try_from(len - 1).unwrap_or(i64::MAX);
    let next = (i64::try_from(cur).unwrap_or(0) + delta).clamp(0, max);
    table.select(Some(usize::try_from(next).unwrap_or(0)));
    vec![]
}

fn nav_to(app: &mut App, target: NavTarget) -> Vec<Effect> {
    let (len, table) = focused_list(app);
    if len == 0 {
        table.select(None);
        return vec![];
    }
    table.select(Some(match target {
        NavTarget::Top => 0,
        NavTarget::Bottom => len - 1,
    }));
    vec![]
}

fn select_first(table: &mut ratatui::widgets::TableState, len: usize) {
    table.select(if len == 0 { None } else { Some(0) });
}

fn loadable_from<T>(result: Result<T, String>) -> Loadable<T> {
    match result {
        Ok(v) => Loadable::Ready(v),
        Err(e) => Loadable::Failed(e),
    }
}

// ── sections ───────────────────────────────────────────────────────────

fn go_section(app: &mut App, section: Section) -> Vec<Effect> {
    app.section = section;
    match section {
        // First visit lazily loads the library.
        Section::Library if matches!(app.library.albums, Loadable::Idle) => {
            reload_albums(app)
        }
        // Liked reloads every visit — it's cheap and ratings change often.
        Section::Liked => {
            app.liked.entries = Loadable::Loading;
            vec![Effect::LoadLiked]
        }
        _ => vec![],
    }
}

fn back(app: &mut App) -> Vec<Effect> {
    if app.overlay == Overlay::Help {
        app.overlay = Overlay::None;
    } else if app.section == Section::Search && app.search.focused {
        app.search.focused = false;
    } else if app.section == Section::Stations && app.stations.focused {
        app.stations.focused = false;
    } else if app.section == Section::Library && app.library.pane == LibraryPane::AlbumDetail {
        app.library.pane = LibraryPane::Albums;
    }
    vec![]
}

fn cycle_kind(app: &mut App, delta: i64) -> Vec<Effect> {
    if app.section != Section::Library || app.library.pane != LibraryPane::Albums {
        // In the search view h/l could plausibly switch buckets; do that.
        if app.section == Section::Search {
            let n = SearchBucket::ALL.len();
            let cur = i64::try_from(app.search.bucket % n).unwrap_or(0);
            let next = (cur + delta).rem_euclid(i64::try_from(n).unwrap_or(1));
            app.search.bucket = usize::try_from(next).unwrap_or(0);
        }
        return vec![];
    }
    let n = i64::try_from(ALBUM_KINDS.len()).unwrap_or(1);
    let cur = i64::try_from(app.library.kind_idx).unwrap_or(0);
    app.library.kind_idx = usize::try_from((cur + delta).rem_euclid(n)).unwrap_or(0);
    reload_albums(app)
}

fn reload_albums(app: &mut App) -> Vec<Effect> {
    app.library.generation += 1;
    app.library.albums = Loadable::Loading;
    vec![Effect::LoadAlbums {
        generation: app.library.generation,
        kind: app.library.kind(),
        size: ALBUM_PAGE,
    }]
}

fn submit_input(app: &mut App) -> Vec<Effect> {
    match app.section {
        Section::Search => {
            let query = app.search.input.value().trim().to_owned();
            if query.is_empty() {
                return vec![];
            }
            app.search.focused = false;
            app.search.generation += 1;
            app.search.results = Loadable::Loading;
            vec![Effect::Search {
                generation: app.search.generation,
                query,
            }]
        }
        Section::Stations => {
            let prompt = app.stations.input.value().trim().to_owned();
            if prompt.is_empty() {
                return vec![];
            }
            app.stations.focused = false;
            app.stations.last_prompt.clone_from(&prompt);
            app.stations.generation += 1;
            app.stations.results = Loadable::Loading;
            vec![Effect::Station {
                generation: app.stations.generation,
                prompt,
                n: STATION_N,
            }]
        }
        _ => vec![],
    }
}

// ── playback ───────────────────────────────────────────────────────────

/// Load-and-play the queue's current track: prefetch hit loads instantly,
/// otherwise a resolve effect goes out and `pending_load` marks the wait.
fn start_current(app: &mut App) -> Vec<Effect> {
    if app.no_audio_device {
        app.set_status("no audio device — playback disabled", true);
        return vec![];
    }
    let Some(idx) = app.queue.current_index() else {
        return vec![];
    };
    let Some(cur) = app.queue.current() else {
        return vec![];
    };
    let (id, duration) = (cur.id.clone(), cur.duration);

    if app.prefetched.as_ref().is_some_and(|(tid, _)| *tid == id) {
        let (tid, bytes) = app.prefetched.take().expect("checked above");
        app.player_load(bytes, tid, duration);
        app.pending_load = None;
        return prefetch_next(app);
    }
    app.pending_load = Some(idx);
    vec![Effect::ResolveAudio {
        queue_index: idx,
        track_id: id,
    }]
}

fn prefetch_next(app: &mut App) -> Vec<Effect> {
    match app.queue.next_up() {
        Some(next) if app.prefetched.as_ref().is_none_or(|(tid, _)| *tid != next.id) => {
            vec![Effect::PrefetchAudio {
                track_id: next.id.clone(),
            }]
        }
        _ => vec![],
    }
}

fn transport_toggle(app: &mut App) -> Vec<Effect> {
    // Idle with a queue: (re)start rather than toggling a dead sink.
    if app.playback.track_id.is_none() && app.pending_load.is_none() {
        if app.queue.current().is_none() && !app.queue.is_empty() {
            app.queue.jump(0);
        }
        if app.queue.current().is_some() {
            return start_current(app);
        }
        return vec![];
    }
    if let Some(p) = &app.player {
        p.toggle();
    }
    vec![]
}

fn next_track(app: &mut App, cause: Advance) -> Vec<Effect> {
    if cause == Advance::Manual {
        note_abandonment(app);
    }
    if app.queue.advance().is_none() {
        // Ran off the end: playback stops naturally; clear transients.
        app.pending_load = None;
        app.player_stop();
        return vec![];
    }
    auto_skip_forward(app);
    if app.queue.current().is_some() {
        start_current(app)
    } else {
        // The auto-skip walked off the end — every remaining track was
        // disliked. Same terminal state as running off naturally.
        app.pending_load = None;
        app.player_stop();
        vec![]
    }
}

/// Dislike auto-skip: when the queue *advances onto* a disliked track
/// (track, album, or artist verdict), walk forward to the first playable
/// one. Direct picks (activating a specific row) never come through here —
/// an explicit choice overrides the dislike, mirroring the web player.
fn auto_skip_forward(app: &mut App) {
    let mut skipped = 0usize;
    while app
        .queue
        .current()
        .is_some_and(|t| signal::is_disliked(t, &app.ratings))
    {
        skipped += 1;
        if app.queue.advance().is_none() {
            break;
        }
    }
    if skipped > 0 {
        app.set_status(format!("auto-skipped {skipped} disliked track(s)"), false);
        sync_queue_cursor(app);
    }
}

fn prev_track(app: &mut App) -> Vec<Effect> {
    // Deep into a track, "previous" means restart (the standard transport
    // behavior); near the start it goes to the prior track.
    if app.playback.position.as_secs() > 3 {
        if let Some(p) = &app.player {
            p.seek_to(std::time::Duration::ZERO);
        }
        return vec![];
    }
    // Walk backward over disliked tracks to the first playable target.
    let not_disliked = |i: &usize| !signal::is_disliked(&app.queue.items()[*i], &app.ratings);
    let target = match app.queue.current_index() {
        Some(cur) => (0..cur).rev().find(not_disliked),
        // Finished queue: "previous" recovers the tail, minus dislikes.
        None => (0..app.queue.len()).rev().find(not_disliked),
    };
    let Some(target) = target else {
        // Nothing playable behind: restart the current track in place (the
        // standard prev-at-start behavior).
        if let Some(p) = &app.player {
            p.seek_to(std::time::Duration::ZERO);
        }
        return vec![];
    };
    note_abandonment(app);
    if app.queue.jump(target).is_some() {
        sync_queue_cursor(app);
        start_current(app)
    } else {
        vec![]
    }
}

fn player_event(app: &mut App, ev: music_player::PlayerEvent) -> Vec<Effect> {
    match ev {
        music_player::PlayerEvent::TrackEnded => next_track(app, Advance::Natural),
        music_player::PlayerEvent::Error(e) => {
            app.set_status(e, true);
            vec![]
        }
    }
}

fn queue_remove_selected(app: &mut App) -> Vec<Effect> {
    let Some(sel) = app.queue_table.selected() else {
        return vec![];
    };
    let was_current = app.queue.current_index() == Some(sel);
    if was_current {
        note_abandonment(app);
    }
    app.queue.remove(sel);
    let len = app.queue.len();
    if len == 0 {
        app.queue_table.select(None);
        if was_current {
            app.player_stop();
            app.pending_load = None;
        }
        return vec![];
    }
    app.queue_table.select(Some(sel.min(len - 1)));
    if was_current {
        // The next track slid into the cursor slot — play it.
        return start_current(app);
    }
    vec![]
}

// ── activate / enqueue / rate ──────────────────────────────────────────

fn activate(app: &mut App) -> Vec<Effect> {
    match app.section {
        Section::Library => match app.library.pane {
            LibraryPane::Albums => open_selected_album(app),
            LibraryPane::AlbumDetail => {
                let Some(sel) = app.library.tracks_table.selected() else {
                    return vec![];
                };
                let Some(album) = app.library.open_album.ready() else {
                    return vec![];
                };
                let queued = album.tracks.iter().map(to_queued).collect();
                note_abandonment(app);
                app.queue.replace(queued, sel);
                app.prefetched = None;
                sync_queue_cursor(app);
                start_current(app)
            }
        },
        Section::Search => activate_search(app),
        Section::Queue => {
            let Some(sel) = app.queue_table.selected() else {
                return vec![];
            };
            note_abandonment(app);
            if app.queue.jump(sel).is_some() {
                start_current(app)
            } else {
                vec![]
            }
        }
        Section::Stations => {
            let Some(sel) = app.stations.table.selected() else {
                return vec![];
            };
            let Some(tracks) = app.stations.results.ready() else {
                return vec![];
            };
            let queued = tracks.iter().map(to_queued).collect();
            note_abandonment(app);
            app.queue.replace(queued, sel);
            app.prefetched = None;
            sync_queue_cursor(app);
            start_current(app)
        }
        Section::Liked => {
            let Some(sel) = app.liked.table.selected() else {
                return vec![];
            };
            let Some(entries) = app.liked.entries.ready() else {
                return vec![];
            };
            // Play the liked *tracks* (with metadata) starting from the
            // selected one; album/artist rows aren't directly playable.
            let tracks: Vec<_> = entries
                .iter()
                .filter_map(|e| e.track.as_ref())
                .map(to_queued)
                .collect();
            let Some(entry) = entries.get(sel) else {
                return vec![];
            };
            let Some(track) = entry.track.as_ref() else {
                app.set_status("only tracks are playable from here", false);
                return vec![];
            };
            let start = tracks
                .iter()
                .position(|t| t.id == track.id.as_str())
                .unwrap_or(0);
            note_abandonment(app);
            app.queue.replace(tracks, start);
            app.prefetched = None;
            sync_queue_cursor(app);
            start_current(app)
        }
    }
}

fn open_selected_album(app: &mut App) -> Vec<Effect> {
    let Some(sel) = app.library.albums_table.selected() else {
        return vec![];
    };
    let Some(albums) = app.library.albums.ready() else {
        return vec![];
    };
    let Some(album) = albums.get(sel) else {
        return vec![];
    };
    let id = album.id.clone();
    app.library.pane = LibraryPane::AlbumDetail;
    app.library.open_album = Loadable::Loading;
    app.library.open_target = Some(id.as_str().to_owned());
    app.library.tracks_table.select(None);
    vec![Effect::OpenAlbum { id }]
}

fn activate_search(app: &mut App) -> Vec<Effect> {
    let bucket = app.search.bucket();
    let idx = app.search.bucket % 3;
    let Some(sel) = app.search.tables[idx].selected() else {
        return vec![];
    };
    let Some(results) = app.search.results.ready() else {
        return vec![];
    };
    match bucket {
        SearchBucket::Tracks => {
            if results.tracks.is_empty() {
                return vec![];
            }
            let queued = results.tracks.iter().map(to_queued).collect();
            note_abandonment(app);
            app.queue.replace(queued, sel);
            app.prefetched = None;
            sync_queue_cursor(app);
            start_current(app)
        }
        SearchBucket::Albums => {
            let Some(album) = results.albums.get(sel) else {
                return vec![];
            };
            let id = album.id.clone();
            app.section = Section::Library;
            app.library.pane = LibraryPane::AlbumDetail;
            app.library.open_album = Loadable::Loading;
            app.library.open_target = Some(id.as_str().to_owned());
            app.library.tracks_table.select(None);
            vec![Effect::OpenAlbum { id }]
        }
        SearchBucket::Artists => {
            app.set_status("artist view isn't in the TUI yet — try their albums", false);
            vec![]
        }
    }
}

/// Keep the queue view's cursor on the playing track after a replace/jump.
fn sync_queue_cursor(app: &mut App) {
    app.queue_table.select(app.queue.current_index());
}

fn enqueue_selected(app: &mut App) -> Vec<Effect> {
    match app.section {
        Section::Library => match app.library.pane {
            LibraryPane::Albums => {
                let Some(sel) = app.library.albums_table.selected() else {
                    return vec![];
                };
                let Some((id, name)) = app
                    .library
                    .albums
                    .ready()
                    .and_then(|a| a.get(sel))
                    .map(|a| (a.id.clone(), a.name.clone()))
                else {
                    return vec![];
                };
                app.set_status(format!("fetching {name}…"), false);
                vec![Effect::EnqueueAlbum { id }]
            }
            LibraryPane::AlbumDetail => {
                let track = app.library.tracks_table.selected().and_then(|sel| {
                    app.library
                        .open_album
                        .ready()
                        .and_then(|a| a.tracks.get(sel))
                });
                enqueue_track(app, track.cloned())
            }
        },
        Section::Search => {
            let idx = app.search.bucket % 3;
            let sel = app.search.tables[idx].selected();
            match app.search.bucket() {
                SearchBucket::Tracks => {
                    let track = sel.and_then(|s| {
                        app.search.results.ready().and_then(|r| r.tracks.get(s))
                    });
                    enqueue_track(app, track.cloned())
                }
                SearchBucket::Albums => {
                    let Some((id, name)) = sel
                        .and_then(|s| app.search.results.ready().and_then(|r| r.albums.get(s)))
                        .map(|a| (a.id.clone(), a.name.clone()))
                    else {
                        return vec![];
                    };
                    app.set_status(format!("fetching {name}…"), false);
                    vec![Effect::EnqueueAlbum { id }]
                }
                SearchBucket::Artists => vec![],
            }
        }
        Section::Stations => {
            let track = app.stations.table.selected().and_then(|sel| {
                app.stations.results.ready().and_then(|r| r.get(sel))
            });
            enqueue_track(app, track.cloned())
        }
        Section::Liked => {
            let track = app.liked.table.selected().and_then(|sel| {
                app.liked
                    .entries
                    .ready()
                    .and_then(|e| e.get(sel))
                    .and_then(|e| e.track.as_ref())
            });
            enqueue_track(app, track.cloned())
        }
        Section::Queue => vec![],
    }
}

fn enqueue_track(app: &mut App, track: Option<music_core::Track>) -> Vec<Effect> {
    let Some(track) = track else {
        return vec![];
    };
    app.set_status(format!("queued {}", track.title), false);
    app.queue.enqueue(vec![to_queued(&track)]);
    vec![]
}

/// The (kind, id, label) a rating key applies to in the current context;
/// falls back to the now-playing track.
fn rating_target(app: &App) -> Option<(&'static str, String, String)> {
    let track_target =
        |t: &music_core::Track| ("track", t.id.as_str().to_owned(), t.title.clone());
    match app.section {
        Section::Library => match app.library.pane {
            LibraryPane::Albums => {
                let sel = app.library.albums_table.selected()?;
                let album = app.library.albums.ready()?.get(sel)?;
                Some(("album", album.id.as_str().to_owned(), album.name.clone()))
            }
            LibraryPane::AlbumDetail => {
                let sel = app.library.tracks_table.selected()?;
                Some(track_target(app.library.open_album.ready()?.tracks.get(sel)?))
            }
        },
        Section::Search => {
            let idx = app.search.bucket % 3;
            let sel = app.search.tables[idx].selected()?;
            let r = app.search.results.ready()?;
            match app.search.bucket() {
                SearchBucket::Tracks => Some(track_target(r.tracks.get(sel)?)),
                SearchBucket::Albums => {
                    let a = r.albums.get(sel)?;
                    Some(("album", a.id.as_str().to_owned(), a.name.clone()))
                }
                SearchBucket::Artists => {
                    let a = r.artists.get(sel)?;
                    Some(("artist", a.id.as_str().to_owned(), a.name.clone()))
                }
            }
        }
        Section::Stations => {
            let sel = app.stations.table.selected()?;
            Some(track_target(app.stations.results.ready()?.get(sel)?))
        }
        Section::Liked => {
            let sel = app.liked.table.selected()?;
            let e = app.liked.entries.ready()?.get(sel)?;
            let kind: &'static str = match e.kind.as_str() {
                "album" => "album",
                "artist" => "artist",
                _ => "track",
            };
            let label = e.track.as_ref().map_or_else(|| e.id.clone(), |t| t.title.clone());
            Some((kind, e.id.clone(), label))
        }
        Section::Queue => {
            let sel = app.queue_table.selected()?;
            let item = app.queue.items().get(sel)?;
            Some(("track", item.id.clone(), item.title.clone()))
        }
    }
    .or_else(|| {
        // Fallback: whatever is playing right now.
        let cur = app.queue.current()?;
        Some(("track", cur.id.clone(), cur.title.clone()))
    })
}

fn rate_selected(app: &mut App, verdict: Option<Rating>) -> Vec<Effect> {
    let Some((kind, id, label)) = rating_target(app) else {
        app.set_status("nothing selected to rate", false);
        return vec![];
    };
    let previous = app.ratings.get(&id).copied();
    match verdict {
        Some(r) => {
            app.ratings.insert(id.clone(), r);
        }
        None => {
            app.ratings.remove(&id);
        }
    }
    let note = match verdict {
        Some(Rating::Like) => format!("♥ liked {label}"),
        Some(Rating::Dislike) => format!("✖ disliked {label}"),
        None => format!("cleared rating on {label}"),
    };
    app.set_status(note, false);
    vec![Effect::SetRating {
        kind,
        id,
        verdict,
        previous,
    }]
}

fn recommend_from_now_playing(app: &mut App) -> Vec<Effect> {
    let seed = app
        .playback
        .track_id
        .clone()
        .or_else(|| app.queue.current().map(|t| t.id.clone()));
    let Some(seed) = seed else {
        app.set_status("nothing playing to seed recommendations from", false);
        return vec![];
    };
    app.set_status("finding similar tracks…", false);
    vec![Effect::RecommendNext {
        seed,
        n: RECOMMEND_N,
    }]
}
