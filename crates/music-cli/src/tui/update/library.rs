//! Library-section reducer logic: the browse modes (albums / artists /
//! tracks), the artist-detail pane, and the album-detail station + "you
//! might like" extras. Split from the dispatcher (`super`) and the shared
//! browse cluster (`super::browse`) so both stay small.
//!
//! Panes and their selectable lists:
//! - [`LibraryPane::Browse`] → one of `albums_table` / `artists_table` /
//!   `songs_table` depending on `mode`.
//! - [`LibraryPane::AlbumDetail`] → a *flat* selection over the album's
//!   tracks followed by the similar-albums/artists footer, indexed by
//!   `tracks_table` (see [`album_detail_row`]).
//! - [`LibraryPane::ArtistDetail`] → a flat selection over albums then top
//!   songs, indexed by `artist_table` (see [`ArtistDetailState::rows`]).

use music_core::Track;
use ratatui::widgets::TableState;

use crate::tui::msg::{Effect, StationError};
use crate::tui::state::{
    App, ArtistRow, LIBRARY_MODES, LibraryMode, LibraryPane, Loadable, SimilarEntry, SimilarKind,
    to_queued,
};

use super::{loaded_len, playback, select_first};

// Tracks mode loads a single full-library page (see `effects::SONGS_PAGE`);
// deeper paging and the recent/most-played/random highlight sub-kinds are a
// deferred follow-up. Station / similar sizing lives in `effects` too.

// ── browse modes ────────────────────────────────────────────────────────

/// `[` / `]` — cycle the browse mode, then load that mode's list.
pub(super) fn cycle_mode(app: &mut App, delta: i64) -> Vec<Effect> {
    // Mode switching only makes sense on the browse list itself; in a detail
    // pane, leave it be (esc backs out first).
    if app.section != crate::tui::state::Section::Library || app.library.pane != LibraryPane::Browse
    {
        return vec![];
    }
    let n = i64::try_from(LIBRARY_MODES.len()).unwrap_or(1);
    let cur = LIBRARY_MODES
        .iter()
        .position(|(m, _)| *m == app.library.mode)
        .unwrap_or(0);
    let next = (i64::try_from(cur).unwrap_or(0) + delta).rem_euclid(n);
    app.library.mode = LIBRARY_MODES[usize::try_from(next).unwrap_or(0)].0;
    reload_browse(app)
}

/// Load the current mode's browse list (always re-issued on switch so a
/// mode can't get stuck on a stale `Loading`). Bumps the shared generation.
pub(super) fn reload_browse(app: &mut App) -> Vec<Effect> {
    app.library.generation += 1;
    let generation = app.library.generation;
    match app.library.mode {
        LibraryMode::Albums => {
            app.library.albums = Loadable::Loading;
            vec![Effect::LoadAlbums {
                generation,
                kind: app.library.kind(),
                size: super::ALBUM_PAGE,
            }]
        }
        LibraryMode::Artists => {
            app.library.artists = Loadable::Loading;
            vec![Effect::LoadArtists { generation }]
        }
        LibraryMode::Tracks => {
            app.library.songs = Loadable::Loading;
            vec![Effect::LoadSongs { generation }]
        }
    }
}

/// True when the current mode's browse list hasn't been loaded yet — the
/// section-entry lazy-load trigger.
pub(super) fn browse_is_idle(app: &App) -> bool {
    match app.library.mode {
        LibraryMode::Albums => matches!(app.library.albums, Loadable::Idle),
        LibraryMode::Artists => matches!(app.library.artists, Loadable::Idle),
        LibraryMode::Tracks => matches!(app.library.songs, Loadable::Idle),
    }
}

/// The (row count, table) the cursor acts on in the browse pane.
pub(super) fn browse_list(app: &mut App) -> (usize, &mut TableState) {
    match app.library.mode {
        LibraryMode::Albums => (
            loaded_len(&app.library.albums),
            &mut app.library.albums_table,
        ),
        LibraryMode::Artists => (
            loaded_len(&app.library.artists),
            &mut app.library.artists_table,
        ),
        LibraryMode::Tracks => (loaded_len(&app.library.songs), &mut app.library.songs_table),
    }
}

// ── activation from the browse list ─────────────────────────────────────

pub(super) fn browse_activate(app: &mut App) -> Vec<Effect> {
    match app.library.mode {
        LibraryMode::Albums => open_selected_album(app),
        LibraryMode::Artists => open_selected_artist(app),
        LibraryMode::Tracks => {
            let Some(sel) = app.library.songs_table.selected() else {
                return vec![];
            };
            let Some(songs) = app.library.songs.ready() else {
                return vec![];
            };
            let queued = songs.iter().map(to_queued).collect();
            playback::play_new_queue(app, queued, sel)
        }
    }
}

fn open_selected_album(app: &mut App) -> Vec<Effect> {
    let Some(id) = app
        .library
        .albums_table
        .selected()
        .and_then(|sel| app.library.albums.ready().and_then(|a| a.get(sel)))
        .map(|a| a.id.clone())
    else {
        return vec![];
    };
    open_album(app, id)
}

fn open_selected_artist(app: &mut App) -> Vec<Effect> {
    let Some((id, name)) = app
        .library
        .artists_table
        .selected()
        .and_then(|sel| app.library.artists.ready().and_then(|a| a.get(sel)))
        .map(|a| (a.id.as_str().to_owned(), a.name.clone()))
    else {
        return vec![];
    };
    open_artist(app, id, name)
}

/// Open an album's detail pane (also used by Search-Albums and the similar
/// footer). Switches to the Library section.
pub(super) fn open_album(app: &mut App, id: music_core::AlbumId) -> Vec<Effect> {
    app.section = crate::tui::state::Section::Library;
    app.library.pane = LibraryPane::AlbumDetail;
    app.library.open_album = Loadable::Loading;
    app.library.album_similar = Loadable::Idle;
    app.library.open_target = Some(id.as_str().to_owned());
    app.library.tracks_table.select(None);
    vec![Effect::OpenAlbum { id }]
}

/// Open an artist's detail pane (from the Artists list, Search-Artists, or a
/// similar-artist footer row). Switches to the Library section.
pub(super) fn open_artist(app: &mut App, id: String, name: String) -> Vec<Effect> {
    app.section = crate::tui::state::Section::Library;
    app.library.pane = LibraryPane::ArtistDetail;
    app.library.open_artist = Loadable::Loading;
    app.library.artist_target = Some(id.clone());
    app.library.artist_table.select(None);
    vec![Effect::OpenArtist { id, name }]
}

pub(super) fn browse_enqueue(app: &mut App) -> Vec<Effect> {
    match app.library.mode {
        LibraryMode::Albums => {
            let Some((id, name)) = app
                .library
                .albums_table
                .selected()
                .and_then(|sel| app.library.albums.ready().and_then(|a| a.get(sel)))
                .map(|a| (a.id.clone(), a.name.clone()))
            else {
                return vec![];
            };
            app.set_status(format!("fetching {name}…"), false);
            vec![Effect::EnqueueAlbum { id }]
        }
        LibraryMode::Artists => {
            app.set_status("open an artist to enqueue their albums or songs", false);
            vec![]
        }
        LibraryMode::Tracks => {
            let track = app
                .library
                .songs_table
                .selected()
                .and_then(|sel| app.library.songs.ready().and_then(|s| s.get(sel)))
                .cloned();
            let Some(track) = track else {
                return vec![];
            };
            app.set_status(format!("queued {}", track.title), false);
            playback::enqueue_tracks(app, vec![to_queued(&track)], false)
        }
    }
}

/// The browse track under the cursor (Tracks mode only) — rate /
/// add-to-playlist / play-next.
pub(super) fn browse_selected_track(app: &App) -> Option<Track> {
    if app.library.mode != LibraryMode::Tracks {
        return None;
    }
    let sel = app.library.songs_table.selected()?;
    app.library.songs.ready()?.get(sel).cloned()
}

// ── album detail (tracks + similar footer, flat selection) ──────────────

/// One selectable row of the album-detail pane.
pub(super) enum AlbumDetailRow<'a> {
    Track(&'a Track),
    Similar(&'a SimilarEntry),
}

/// Combined selectable length of the album-detail pane: album tracks first,
/// then the "you might like" footer entries.
pub(super) fn album_detail_len(app: &App) -> usize {
    let tracks = app.library.open_album.ready().map_or(0, |a| a.tracks.len());
    tracks + loaded_len(&app.library.album_similar)
}

/// Resolve a flat album-detail index to a track or a similar-footer row.
pub(super) fn album_detail_row(app: &App, idx: usize) -> Option<AlbumDetailRow<'_>> {
    let album = app.library.open_album.ready()?;
    if let Some(t) = album.tracks.get(idx) {
        return Some(AlbumDetailRow::Track(t));
    }
    let footer_idx = idx - album.tracks.len();
    app.library
        .album_similar
        .ready()
        .and_then(|s| s.get(footer_idx))
        .map(AlbumDetailRow::Similar)
}

pub(super) fn album_activate(app: &mut App) -> Vec<Effect> {
    let Some(sel) = app.library.tracks_table.selected() else {
        return vec![];
    };
    match album_detail_row(app, sel) {
        Some(AlbumDetailRow::Track(_)) => {
            // Play the album from the selected track.
            let Some(album) = app.library.open_album.ready() else {
                return vec![];
            };
            let queued = album.tracks.iter().map(to_queued).collect();
            playback::play_new_queue(app, queued, sel)
        }
        Some(AlbumDetailRow::Similar(entry)) => navigate_similar(app, &entry.clone()),
        None => vec![],
    }
}

fn navigate_similar(app: &mut App, entry: &SimilarEntry) -> Vec<Effect> {
    match entry.kind {
        SimilarKind::Album => open_album(app, music_core::AlbumId::from(entry.id.clone())),
        SimilarKind::Artist => open_artist(app, entry.id.clone(), entry.name.clone()),
    }
}

pub(super) fn album_enqueue(app: &mut App) -> Vec<Effect> {
    let Some(track) = album_selected_track(app) else {
        // A similar-footer row selected: enqueue nothing (navigate with enter).
        return vec![];
    };
    app.set_status(format!("queued {}", track.title), false);
    let queued = to_queued(&track);
    playback::enqueue_tracks(app, vec![queued], false)
}

/// The album-detail track under the cursor, or `None` when a similar-footer
/// row is selected. Drives rate / add-to-playlist / play-next.
pub(super) fn album_selected_track(app: &App) -> Option<Track> {
    let sel = app.library.tracks_table.selected()?;
    match album_detail_row(app, sel)? {
        AlbumDetailRow::Track(t) => Some(t.clone()),
        AlbumDetailRow::Similar(_) => None,
    }
}

// ── artist detail (albums + top songs, flat selection) ──────────────────

pub(super) fn artist_detail_len(app: &App) -> usize {
    app.library.open_artist.ready().map_or(0, |a| a.rows.len())
}

pub(super) fn artist_activate(app: &mut App) -> Vec<Effect> {
    let Some(sel) = app.library.artist_table.selected() else {
        return vec![];
    };
    let Some(detail) = app.library.open_artist.ready() else {
        return vec![];
    };
    match detail.rows.get(sel) {
        Some(ArtistRow::Album(album)) => {
            let id = album.id.clone();
            open_album(app, id)
        }
        Some(ArtistRow::Song(song)) => {
            // Play the artist's top songs starting from this one.
            let songs = detail.top_songs();
            let start = songs.iter().position(|t| t.id == song.id).unwrap_or(0);
            let queued = songs.iter().map(to_queued).collect();
            playback::play_new_queue(app, queued, start)
        }
        None => vec![],
    }
}

pub(super) fn artist_enqueue(app: &mut App) -> Vec<Effect> {
    let sel = app.library.artist_table.selected();
    let Some(detail) = app.library.open_artist.ready() else {
        return vec![];
    };
    match sel.and_then(|s| detail.rows.get(s)) {
        Some(ArtistRow::Song(song)) => {
            let track = song.clone();
            app.set_status(format!("queued {}", track.title), false);
            playback::enqueue_tracks(app, vec![to_queued(&track)], false)
        }
        // Enqueue an album row's tracks via the album fetch path.
        Some(ArtistRow::Album(album)) => {
            let id = album.id.clone();
            app.set_status(format!("fetching {}…", album.name), false);
            vec![Effect::EnqueueAlbum { id }]
        }
        None => vec![],
    }
}

/// The artist-detail song under the cursor (song rows only) — rate /
/// add-to-playlist / play-next.
pub(super) fn artist_selected_track(app: &App) -> Option<Track> {
    let sel = app.library.artist_table.selected()?;
    match app.library.open_artist.ready()?.rows.get(sel)? {
        ArtistRow::Song(t) => Some(t.clone()),
        ArtistRow::Album(_) => None,
    }
}

// ── rating target ───────────────────────────────────────────────────────

/// The `(kind, id, label)` a rating key applies to in the Library section:
/// the selected row's entity, resolved per pane/mode. Rating the artist when
/// no more-specific row is selected (an album/song row rates that instead).
pub(super) fn rating_target(app: &App) -> Option<(&'static str, String, String)> {
    let track = |t: &Track| ("track", t.id.as_str().to_owned(), t.title.clone());
    match app.library.pane {
        LibraryPane::Browse => match app.library.mode {
            LibraryMode::Albums => {
                let sel = app.library.albums_table.selected()?;
                let a = app.library.albums.ready()?.get(sel)?;
                Some(("album", a.id.as_str().to_owned(), a.name.clone()))
            }
            LibraryMode::Artists => {
                let sel = app.library.artists_table.selected()?;
                let a = app.library.artists.ready()?.get(sel)?;
                Some(("artist", a.id.as_str().to_owned(), a.name.clone()))
            }
            LibraryMode::Tracks => Some(track(&browse_selected_track(app)?)),
        },
        LibraryPane::AlbumDetail => {
            let sel = app.library.tracks_table.selected()?;
            match album_detail_row(app, sel)? {
                AlbumDetailRow::Track(t) => Some(track(t)),
                AlbumDetailRow::Similar(e) => {
                    let kind = match e.kind {
                        SimilarKind::Album => "album",
                        SimilarKind::Artist => "artist",
                    };
                    Some((kind, e.id.clone(), e.name.clone()))
                }
            }
        }
        LibraryPane::ArtistDetail => {
            let detail = app.library.open_artist.ready()?;
            match app
                .library
                .artist_table
                .selected()
                .and_then(|s| detail.rows.get(s))
            {
                Some(ArtistRow::Song(t)) => Some(track(t)),
                Some(ArtistRow::Album(a)) => {
                    Some(("album", a.id.as_str().to_owned(), a.name.clone()))
                }
                // No row (or nothing selected): rate the artist.
                _ => Some((
                    "artist",
                    detail.artist.id.as_str().to_owned(),
                    detail.artist.name.clone(),
                )),
            }
        }
    }
}

// ── album / artist station (S) ──────────────────────────────────────────

/// `S` — start a station from the open album (its tracks) or artist (its top
/// songs). Replaces the queue with the resolved recommendations.
pub(super) fn album_station(app: &mut App) -> Vec<Effect> {
    let seeds: Vec<String> = match app.library.pane {
        LibraryPane::AlbumDetail => app
            .library
            .open_album
            .ready()
            .map(|a| a.tracks.iter().map(|t| t.id.as_str().to_owned()).collect())
            .unwrap_or_default(),
        LibraryPane::ArtistDetail => app
            .library
            .open_artist
            .ready()
            .map(|a| {
                a.top_songs()
                    .iter()
                    .map(|t| t.id.as_str().to_owned())
                    .collect()
            })
            .unwrap_or_default(),
        LibraryPane::Browse => Vec::new(),
    };
    if seeds.is_empty() {
        app.set_status("no tracks to seed a station from", false);
        return vec![];
    }
    app.set_status("starting a station…", false);
    vec![Effect::AlbumStation {
        candidate_seeds: seeds,
    }]
}

// ── completions ─────────────────────────────────────────────────────────

pub(super) fn on_artists_loaded(
    app: &mut App,
    generation: u64,
    result: Result<Vec<music_core::Artist>, String>,
) {
    if generation != app.library.generation {
        return;
    }
    app.library.artists = super::loadable_from(result);
    select_first(
        &mut app.library.artists_table,
        loaded_len(&app.library.artists),
    );
}

pub(super) fn on_songs_loaded(app: &mut App, generation: u64, result: Result<Vec<Track>, String>) {
    if generation != app.library.generation {
        return;
    }
    app.library.songs = super::loadable_from(result);
    select_first(&mut app.library.songs_table, loaded_len(&app.library.songs));
}

pub(super) fn on_artist_opened(
    app: &mut App,
    id: &str,
    result: Result<crate::tui::state::ArtistDetailState, String>,
) {
    if app.library.artist_target.as_deref() != Some(id) {
        return;
    }
    app.library.open_artist = super::loadable_from(result);
    let len = artist_detail_len(app);
    select_first(&mut app.library.artist_table, len);
}

/// After an album's tracks land, kick off the "you might like" footer
/// (needs the tracks as seeds). No-op if the album failed to load.
pub(super) fn after_album_opened(app: &mut App) -> Vec<Effect> {
    let Some(album) = app.library.open_album.ready() else {
        return vec![];
    };
    let seed_track_ids: Vec<String> = album
        .tracks
        .iter()
        .map(|t| t.id.as_str().to_owned())
        .collect();
    if seed_track_ids.is_empty() {
        return vec![];
    }
    let album_id = album.album.id.as_str().to_owned();
    let artist_id = album
        .album
        .artist_id
        .as_ref()
        .map(|a| a.as_str().to_owned());
    app.library.album_similar = Loadable::Loading;
    vec![Effect::LoadAlbumSimilar {
        album_id,
        artist_id,
        seed_track_ids,
    }]
}

pub(super) fn on_album_similar(
    app: &mut App,
    album_id: &str,
    result: Result<Vec<SimilarEntry>, String>,
) {
    // Guard against a stale footer for a since-closed album.
    if app.library.open_target.as_deref() != Some(album_id) {
        return;
    }
    app.library.album_similar = super::loadable_from(result);
}

pub(super) fn on_album_station(
    app: &mut App,
    result: Result<Vec<Track>, StationError>,
) -> Vec<Effect> {
    match result {
        Ok(tracks) if tracks.is_empty() => {
            app.set_status("no station tracks found for this seed", false);
            vec![]
        }
        Ok(tracks) => {
            let n = tracks.len();
            let queued: Vec<_> = tracks.iter().map(to_queued).collect();
            app.set_status(format!("station: {n} track(s)"), false);
            playback::play_new_queue(app, queued, 0)
        }
        Err(StationError::Unavailable) => {
            app.set_status(
                "station unavailable — recommender warming up or album not embedded yet",
                true,
            );
            vec![]
        }
        Err(StationError::Other(e)) => {
            app.set_status(format!("station failed: {e}"), true);
            vec![]
        }
    }
}
