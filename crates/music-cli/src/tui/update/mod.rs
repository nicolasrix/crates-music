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

mod autoplay;
mod browse;
mod downloads;
mod library;
mod playback;
mod playlists;
mod room;
mod settings;

#[cfg(test)]
mod autoplay_tests;
#[cfg(test)]
mod downloads_tests;
#[cfg(test)]
mod settings_tests;
#[cfg(test)]
mod library_tests;
#[cfg(test)]
mod playlist_tests;
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
            let mut effects = playback::signal_tick(app);
            effects.extend(autoplay::maybe_refill(app));
            effects
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
        // h / l: adjust the selected settings row when in Settings; otherwise
        // cycle the library album-list kind / search result bucket.
        Msg::CycleKindPrev if app.section == Section::Settings => settings::adjust(app, -1),
        Msg::CycleKindNext if app.section == Section::Settings => settings::adjust(app, 1),
        Msg::CycleKindPrev => cycle_kind(app, -1),
        Msg::CycleKindNext => cycle_kind(app, 1),
        Msg::CycleModePrev => library::cycle_mode(app, -1),
        Msg::CycleModeNext => library::cycle_mode(app, 1),
        Msg::AlbumStation => library::album_station(app),
        Msg::Activate => browse::activate(app),
        Msg::Enqueue => browse::enqueue_selected(app),
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
        Msg::PlayNext => browse::play_next_selected(app),
        Msg::ToggleOutput => room::toggle_output(app),
        Msg::ToggleAutoplay => autoplay::toggle(app),
        Msg::Feedback(vote) => autoplay::feedback(app, vote),
        Msg::SaveOffline => downloads::save_offline(app),
        Msg::BulkDownload => downloads::bulk_download(app),
        Msg::EvictCache => downloads::evict(app),
        Msg::Player(ev) => playback::player_event(app, ev),
        Msg::Sync(ev) => room::handle(app, ev),

        // ── playlists (gestures + overlays) ───────────────────────────
        Msg::AddToPlaylist => playlists::open_picker(app),
        Msg::NewPlaylist => playlists::new_playlist_prompt(app),
        Msg::PlaylistShufflePlay => playlists::shuffle_play(app),
        Msg::PlaylistRenamePrompt => playlists::rename_prompt(app),
        Msg::PlaylistDelete => playlists::delete(app),
        Msg::PlaylistRemoveTrack => playlists::remove_track(app),
        Msg::PlaylistSuggest => playlists::suggest(app),
        Msg::PickerMove(d) => playlists::picker_move(app, d),
        Msg::PickerActivate => playlists::picker_activate(app),
        Msg::PickerClose => playlists::picker_close(app),
        Msg::PromptInput(im) => playlists::prompt_input(app, &im),
        Msg::PromptSubmit => playlists::prompt_submit(app),
        Msg::PromptClose => playlists::prompt_close(app),

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
                let len = library::album_detail_len(app);
                select_first(&mut app.library.tracks_table, len);
                // A loaded album kicks off its "you might like" footer.
                return library::after_album_opened(app);
            }
            vec![]
        }
        Msg::ArtistsLoaded { generation, result } => {
            library::on_artists_loaded(app, generation, result);
            vec![]
        }
        Msg::SongsLoaded { generation, result } => {
            library::on_songs_loaded(app, generation, result);
            vec![]
        }
        Msg::ArtistOpened { id, result } => {
            library::on_artist_opened(app, &id, result);
            vec![]
        }
        Msg::AlbumSimilarLoaded { album_id, result } => {
            library::on_album_similar(app, &album_id, result);
            vec![]
        }
        Msg::AlbumStationDone { result } => library::on_album_station(app, result),
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
        Msg::DownloadsLoaded { stats, pinned } => downloads::on_loaded(app, stats, pinned),
        Msg::PinDone { note, is_error } => downloads::on_pin_done(app, note, is_error),
        Msg::AutoplayRefilled {
            generation,
            need,
            result,
        } => autoplay::on_refilled(app, generation, need, result),
        Msg::FeedbackDone {
            track_id,
            previous,
            result,
        } => autoplay::on_feedback_done(app, &track_id, previous, result),
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
        Msg::PlaylistsLoaded { generation, result } => {
            playlists::on_loaded(app, generation, result)
        }
        Msg::PlaylistOpened { id, result } => playlists::on_opened(app, &id, result),
        Msg::PlaylistWriteDone {
            note,
            is_error,
            reload_list,
            reopen_id,
        } => playlists::on_write_done(app, note, is_error, reload_list, reopen_id),
        Msg::PlaylistSuggestionsDone {
            playlist_id,
            result,
        } => playlists::on_suggestions(app, &playlist_id, result),

        // ── settings ──────────────────────────────────────────────────
        Msg::SettingsSaved { result } => settings::on_saved(app, result),
        Msg::SignedOut { result } => settings::on_signed_out(app, result),
        Msg::CacheInvalidated { result } => settings::on_cache_invalidated(app, result),
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
            LibraryPane::Browse => library::browse_list(app),
            LibraryPane::AlbumDetail => {
                let len = library::album_detail_len(app);
                (len, &mut app.library.tracks_table)
            }
            LibraryPane::ArtistDetail => {
                let len = library::artist_detail_len(app);
                (len, &mut app.library.artist_table)
            }
        },
        Section::Search => {
            let lens = search_bucket_lens(app);
            let idx = app.search.bucket % 3;
            (lens[idx], &mut app.search.tables[idx])
        }
        Section::Queue => (app.queue.len(), &mut app.queue_table),
        Section::Playlists => playlists::focused_list(app),
        Section::Stations => (
            loaded_len(&app.stations.results),
            &mut app.stations.table,
        ),
        Section::Liked => (loaded_len(&app.liked.entries), &mut app.liked.table),
        Section::Downloads => (
            loaded_len(&app.downloads.pinned),
            &mut app.downloads.table,
        ),
        Section::Settings => {
            let len = settings::row_count(app);
            (len, &mut app.settings.table)
        }
    }
}

fn search_bucket_lens(app: &App) -> [usize; 3] {
    app.search.results.ready().map_or([0; 3], |r| {
        [r.tracks.len(), r.albums.len(), r.artists.len()]
    })
}

pub(super) fn loaded_len<T>(l: &Loadable<Vec<T>>) -> usize {
    l.ready().map_or(0, Vec::len)
}

fn nav(app: &mut App, delta: i64) -> Vec<Effect> {
    // Moving the cursor off the armed sign-out row disarms it — the two-step
    // guard must survive intervening navigation, not just other actions.
    app.settings.confirm_signout = false;
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
    app.settings.confirm_signout = false;
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

pub(super) fn select_first(table: &mut ratatui::widgets::TableState, len: usize) {
    table.select(if len == 0 { None } else { Some(0) });
}

pub(super) fn loadable_from<T>(result: Result<T, String>) -> Loadable<T> {
    match result {
        Ok(v) => Loadable::Ready(v),
        Err(e) => Loadable::Failed(e),
    }
}

// ── sections ───────────────────────────────────────────────────────────

fn go_section(app: &mut App, section: Section) -> Vec<Effect> {
    app.section = section;
    match section {
        // First visit (or retry after a failed load) lazily loads the
        // current browse mode's list.
        Section::Library if library::browse_needs_load(app) => library::reload_browse(app),
        // Liked reloads every visit — it's cheap and ratings change often.
        Section::Liked => {
            app.liked.entries = Loadable::Loading;
            vec![Effect::LoadLiked]
        }
        // First visit lazily loads the playlist list.
        Section::Playlists if matches!(app.playlists.list, Loadable::Idle) => {
            playlists::reload_list(app)
        }
        // Downloads reloads every visit — cache totals + pin state drift as
        // tracks auto-cache and `d` runs elsewhere; the reads are local.
        Section::Downloads => downloads::reload(app),
        // Settings refreshes identity (account card + admin gating).
        Section::Settings => settings::reload(app),
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
    } else if app.section == Section::Library
        && matches!(
            app.library.pane,
            LibraryPane::AlbumDetail | LibraryPane::ArtistDetail
        )
    {
        // Return to wherever this detail was opened from (Search / Liked / a
        // parent detail pane), else fall back to the browse list.
        match app.library.nav_return.take() {
            Some((section, pane)) => {
                app.section = section;
                app.library.pane = pane;
            }
            None => app.library.pane = LibraryPane::Browse,
        }
    } else if app.section == Section::Playlists {
        playlists::back(app);
    }
    vec![]
}

fn cycle_kind(app: &mut App, delta: i64) -> Vec<Effect> {
    // h/l only cycles the album-list kind in Albums-mode browse.
    let in_albums_browse = app.section == Section::Library
        && app.library.pane == LibraryPane::Browse
        && app.library.mode == crate::tui::state::LibraryMode::Albums;
    if !in_albums_browse {
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
    library::reload_browse(app)
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

/// The (kind, id, label) a rating key applies to in the current context;
/// falls back to the now-playing track.
fn rating_target(app: &App) -> Option<(&'static str, String, String)> {
    let track_target =
        |t: &music_core::Track| ("track", t.id.as_str().to_owned(), t.title.clone());
    match app.section {
        Section::Library => library::rating_target(app),
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
            let label = e.track.as_ref().map_or_else(
                || e.label.clone().unwrap_or_else(|| e.id.clone()),
                |t| t.title.clone(),
            );
            Some((kind, e.id.clone(), label))
        }
        Section::Queue => {
            let sel = app.queue_table.selected()?;
            let item = app.queue.items().get(sel)?;
            Some(("track", item.id.clone(), item.title.clone()))
        }
        Section::Playlists => Some(track_target(&playlists::selected_track(app)?)),
        Section::Downloads => Some(track_target(&downloads::selected_track(app)?)),
        // Settings lists no tracks; rating keys fall through to now-playing.
        Section::Settings => None,
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
