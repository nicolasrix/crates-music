//! The reducer: `update(&mut App, Msg) -> Vec<Effect>`. Pure over app state
//! plus (deliberately) direct calls into the `Player` handle — those are
//! fire-and-forget channel sends, safe and instant, and threading them
//! through effects would only add latency to keypresses.
//!
//! Split by concern: this module owns the dispatcher plus browse-shaped
//! state (navigation, sections, search/library/liked, ratings);
//! [`playback`] owns the local transport/queue/signal logic; [`room`] owns
//! the sync-room integration (server frames, projection, op submission).
//! Every queue gesture funnels through a `playback` entry point, which
//! forks to `room` while the sync connection is online.

mod playback;
mod room;

#[cfg(test)]
mod room_tests;
#[cfg(test)]
mod tests;

use playback::{Advance, MoveKind};

use super::msg::{Effect, Msg, StationError};
use super::state::{
    ALBUM_KINDS, App, LibraryPane, Loadable, Overlay, Rating, SearchBucket, Section, to_queued,
};

const RECOMMEND_N: usize = 20;
const STATION_N: usize = 30;
const ALBUM_PAGE: u32 = 100;

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
            playback::signal_tick(app)
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
        Msg::TransportToggle => playback::transport_toggle(app),
        Msg::TransportNext => playback::next_track(app, Advance::Manual),
        Msg::TransportPrev => playback::prev_track(app),
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
        Msg::QueueRemoveSelected => playback::queue_remove_selected(app),
        Msg::QueueClear => playback::queue_clear_upcoming(app),
        Msg::QueueMoveDown => playback::queue_move(app, MoveKind::Down),
        Msg::QueueMoveUp => playback::queue_move(app, MoveKind::Up),
        Msg::QueueMoveTop => playback::queue_move(app, MoveKind::Top),
        Msg::PlayNext => play_next_selected(app),
        Msg::ToggleOutput => room::toggle_output(app),
        Msg::Player(ev) => playback::player_event(app, ev),
        Msg::Sync(ev) => room::handle(app, ev),

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
        Msg::AlbumTracksForEnqueue { result } => match result {
            Ok(album) => {
                let n = album.tracks.len();
                let queued: Vec<_> = album.tracks.iter().map(to_queued).collect();
                app.set_status(
                    format!("queued {n} track(s) from {}", album.album.name),
                    false,
                );
                playback::enqueue_tracks(app, queued, false)
            }
            Err(e) => {
                app.set_status(format!("enqueue failed: {e}"), true);
                vec![]
            }
        },
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
        Msg::RecommendDone { result } => match result {
            Ok(tracks) => {
                let n = tracks.len();
                let queued: Vec<_> = tracks.iter().map(to_queued).collect();
                app.set_status(format!("queued {n} similar track(s)"), false);
                playback::enqueue_tracks(app, queued, false)
            }
            Err(StationError::Unavailable) => {
                app.set_status(
                    "recommendations unavailable — recommender warming up or seed not embedded",
                    true,
                );
                vec![]
            }
            Err(StationError::Other(e)) => {
                app.set_status(format!("recommend failed: {e}"), true);
                vec![]
            }
        },
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
            // Stale if the queue moved on (or, online, the projection
            // shifted) while the fetch was in flight. Clear a matching
            // pending_load so the room's follow logic can re-issue the
            // right resolve instead of waiting on one that never lands.
            let current = app.queue.current();
            if app.queue.current_index() != Some(queue_index)
                || current.is_none_or(|t| t.id != track_id)
            {
                if app.pending_load == Some(queue_index) {
                    app.pending_load = None;
                }
                return if app.sync.online() { room::follow(app) } else { vec![] };
            }
            // Silent remote: output was toggled off after this resolve was
            // issued. `player_load` would start the sink, so drop the bytes
            // rather than break the "no sound on this device" guarantee.
            if app.sync.online() && !app.sync.output_on {
                app.pending_load = None;
                return vec![];
            }
            let duration = current.and_then(|t| t.duration);
            app.player_load(bytes, track_id, duration);
            app.pending_load = None;
            playback::prefetch_next(app)
        }
        Msg::AudioFailed {
            queue_index,
            track_id,
            error,
        } => {
            if app.queue.current_index() != Some(queue_index)
                || app.queue.current().is_none_or(|t| t.id != track_id)
            {
                if app.pending_load == Some(queue_index) {
                    app.pending_load = None;
                }
                return if app.sync.online() { room::follow(app) } else { vec![] };
            }
            app.pending_load = None;
            app.set_status(format!("skipping {track_id}: {error}"), true);
            // Not a user abandonment — the track never played.
            playback::next_track(app, Advance::Natural)
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
                playback::requeue_failed_events(app, events, &e);
            }
            vec![]
        }
        Msg::WhoamiLoaded { result } => {
            match result {
                Ok(info) => app.whoami = Some(info),
                // Cosmetic (role gating fails open to server enforcement);
                // not worth a status line at boot.
                Err(e) => tracing::debug!(error = %e, "whoami fetch failed"),
            }
            vec![]
        }
        Msg::TracksHydrated { ids, result } => {
            for id in &ids {
                app.sync.hydrating.remove(id);
            }
            match result {
                Ok(tracks) => {
                    let resolved: std::collections::HashSet<&str> =
                        tracks.iter().map(|t| t.id.as_str()).collect();
                    // Ids we asked for but didn't get back are unresolvable
                    // (deleted / not a song) — remember them so hydration
                    // doesn't re-request on every inbound frame.
                    for id in &ids {
                        if !resolved.contains(id.as_str()) {
                            app.sync.hydrate_failed.insert(id.clone());
                        }
                    }
                    for t in &tracks {
                        app.sync.meta.insert(t.id.as_str().to_owned(), to_queued(t));
                    }
                    if app.sync.online() {
                        // If the *current* track's metadata just arrived, its
                        // album/artist dislike couldn't be known when it
                        // became current — force one re-classification so a
                        // now-known dislike is honored (project alone would
                        // leave `last_classified` latched and skip it).
                        if app
                            .sync
                            .cursor_track_id()
                            .is_some_and(|tid| ids.iter().any(|id| id == tid))
                        {
                            app.sync.last_classified = None;
                        }
                        room::project(app);
                        return room::follow(app);
                    }
                }
                // A whole-batch failure: hold every id so it isn't retried
                // in a hot loop (cleared on the next queue-growth op).
                Err(e) => {
                    tracing::debug!(error = %e, "queue hydration failed");
                    for id in &ids {
                        app.sync.hydrate_failed.insert(id.clone());
                    }
                }
            }
            vec![]
        }
    }
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
                playback::play_new_queue(app, queued, sel)
            }
        },
        Section::Search => activate_search(app),
        Section::Queue => {
            let Some(sel) = app.queue_table.selected() else {
                return vec![];
            };
            if app.sync.online() {
                return room::jump_selected(app, sel);
            }
            let next_id = app.queue.items().get(sel).map(|t| t.id.clone());
            playback::note_abandonment(app, next_id.as_deref());
            if app.queue.jump(sel).is_some() {
                playback::start_current(app)
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
            playback::play_new_queue(app, queued, sel)
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
            playback::play_new_queue(app, tracks, start)
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
            playback::play_new_queue(app, queued, sel)
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
    let queued = to_queued(&track);
    playback::enqueue_tracks(app, vec![queued], false)
}

/// `P` — in the queue view, move the selected row to right after the
/// cursor; in track lists, enqueue the selected track there.
fn play_next_selected(app: &mut App) -> Vec<Effect> {
    if app.section == Section::Queue {
        return playback::queue_move(app, MoveKind::AfterCursor);
    }
    let Some(track) = selected_track(app) else {
        app.set_status("play-next works on track rows", false);
        return vec![];
    };
    app.set_status(format!("playing {} next", track.title), false);
    let queued = to_queued(&track);
    playback::enqueue_tracks(app, vec![queued], true)
}

/// The selected row's track, in sections that list tracks.
fn selected_track(app: &App) -> Option<music_core::Track> {
    match app.section {
        Section::Library => match app.library.pane {
            LibraryPane::AlbumDetail => {
                let sel = app.library.tracks_table.selected()?;
                app.library.open_album.ready()?.tracks.get(sel).cloned()
            }
            LibraryPane::Albums => None,
        },
        Section::Search => {
            let idx = app.search.bucket % 3;
            let sel = app.search.tables[idx].selected()?;
            match app.search.bucket() {
                SearchBucket::Tracks => app.search.results.ready()?.tracks.get(sel).cloned(),
                _ => None,
            }
        }
        Section::Stations => {
            let sel = app.stations.table.selected()?;
            app.stations.results.ready()?.get(sel).cloned()
        }
        Section::Liked => {
            let sel = app.liked.table.selected()?;
            app.liked.entries.ready()?.get(sel)?.track.clone()
        }
        Section::Queue => None,
    }
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
