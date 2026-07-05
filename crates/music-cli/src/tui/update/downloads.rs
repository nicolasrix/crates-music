//! Downloads / offline reducer logic: the `d` save-offline toggle, `W` bulk
//! download (album / playlist / warm-from-liked), `E` evict, plus the section
//! load + play-a-pinned-track path. Pure over app state — every cache
//! operation rides an [`Effect`]; the reducer only decides *what* to pin and
//! records the resulting status.

use music_cache::AudioCacheStats;
use music_player::QueuedTrack;

use crate::tui::msg::Effect;
use crate::tui::state::{
    App, LibraryPane, Loadable, PinnedRow, PlaylistsPane, Section, to_queued,
};

use super::playback;

/// Reload the section: cache totals + the pinned table. Both go `Loading`
/// first so a slow hydrate shows a spinner rather than stale rows.
pub(super) fn reload(app: &mut App) -> Vec<Effect> {
    app.downloads.stats = Loadable::Loading;
    app.downloads.pinned = Loadable::Loading;
    vec![Effect::LoadDownloads]
}

/// The (track_id, title) `d` targets: the selected pinned row in the Downloads
/// section, else the contextual track row (shared with add-to-playlist).
fn download_target(app: &App) -> Option<(String, String)> {
    if app.section == Section::Downloads {
        let sel = app.downloads.table.selected()?;
        let row = app.downloads.pinned.ready()?.get(sel)?;
        let title = row
            .track
            .as_ref()
            .map_or_else(|| row.track_id.clone(), |t| t.title.clone());
        return Some((row.track_id.clone(), title));
    }
    super::browse::add_target(app)
}

/// `d` — toggle save-offline for the contextual track. The effect pins
/// (fetch-if-missing) or unpins atomically; here we only fire it.
pub(super) fn save_offline(app: &mut App) -> Vec<Effect> {
    let Some((track_id, title)) = download_target(app) else {
        app.set_status("no track here to save offline", false);
        return vec![];
    };
    app.set_status(format!("{title}: updating offline…"), false);
    vec![Effect::PinToggle { track_id, title }]
}

/// `W` — bulk-download the contextual collection: the open album, the open
/// playlist, or (in the Downloads section) the whole liked-tracks set.
pub(super) fn bulk_download(app: &mut App) -> Vec<Effect> {
    match app.section {
        Section::Downloads => {
            app.set_status("warming cache from liked…", false);
            vec![Effect::WarmLiked]
        }
        Section::Library if app.library.pane == LibraryPane::AlbumDetail => {
            let Some(album) = app.library.open_album.ready() else {
                app.set_status("album still loading", false);
                return vec![];
            };
            let ids: Vec<String> = album.tracks.iter().map(|t| t.id.as_str().to_owned()).collect();
            let name = album.album.name.clone();
            bulk(app, ids, &name)
        }
        Section::Playlists if app.playlists.pane == PlaylistsPane::Detail => {
            let Some(pl) = app.playlists.open.ready() else {
                app.set_status("playlist still loading", false);
                return vec![];
            };
            let ids = pl.track_ids.clone();
            let name = pl.summary.name.clone();
            bulk(app, ids, &name)
        }
        _ => {
            app.set_status(
                "bulk download works on an album, a playlist, or the Downloads page",
                false,
            );
            vec![]
        }
    }
}

/// Shared album/playlist bulk-pin: guard the empty case, set a progress line,
/// fire the effect.
fn bulk(app: &mut App, ids: Vec<String>, name: &str) -> Vec<Effect> {
    if ids.is_empty() {
        app.set_status("nothing to download here", false);
        return vec![];
    }
    app.set_status(format!("downloading {name} ({} tracks)…", ids.len()), false);
    vec![Effect::PinBulk {
        track_ids: ids,
        label: format!("download {name}"),
    }]
}

/// `E` — fit the regular cache to its budget (Downloads only; keymap-gated).
pub(super) fn evict(app: &mut App) -> Vec<Effect> {
    app.set_status("evicting to budget…", false);
    vec![Effect::EvictCache]
}

/// Enter on a pinned row: play the pinned set from that row. Rows lacking
/// hydrated metadata (offline) still play — the queue keys on the track id.
pub(super) fn activate(app: &mut App) -> Vec<Effect> {
    let Some(sel) = app.downloads.table.selected() else {
        return vec![];
    };
    let Some(rows) = app.downloads.pinned.ready() else {
        return vec![];
    };
    if rows.is_empty() {
        return vec![];
    }
    let queued: Vec<QueuedTrack> = rows.iter().map(row_to_queued).collect();
    playback::play_new_queue(app, queued, sel)
}

/// The selected pinned row's track, when hydrated — powers `e` enqueue and the
/// rating / add-to-playlist gestures in the Downloads section.
pub(super) fn selected_track(app: &App) -> Option<music_core::Track> {
    let sel = app.downloads.table.selected()?;
    app.downloads.pinned.ready()?.get(sel)?.track.clone()
}

/// A pinned row → a queue entry: the hydrated track when present, else a
/// minimal id-only entry (title = id) that still resolves audio from cache.
fn row_to_queued(row: &PinnedRow) -> QueuedTrack {
    match &row.track {
        Some(t) => to_queued(t),
        None => QueuedTrack {
            id: row.track_id.clone(),
            title: row.track_id.clone(),
            artist: None,
            album: None,
            artist_id: None,
            album_id: None,
            duration: None,
        },
    }
}

/// The section-load effect completed — populate + select the first row.
pub(super) fn on_loaded(
    app: &mut App,
    stats: Result<AudioCacheStats, String>,
    pinned: Result<Vec<PinnedRow>, String>,
) -> Vec<Effect> {
    app.downloads.stats = super::loadable_from(stats);
    app.downloads.pinned = super::loadable_from(pinned);
    super::select_first(
        &mut app.downloads.table,
        super::loaded_len(&app.downloads.pinned),
    );
    vec![]
}

/// A pin / unpin / bulk / evict op finished: surface its note and, when the
/// Downloads section is on screen, reload it so the totals + pin set refresh.
pub(super) fn on_pin_done(app: &mut App, note: String, is_error: bool) -> Vec<Effect> {
    app.set_status(note, is_error);
    if app.section == Section::Downloads {
        reload(app)
    } else {
        vec![]
    }
}
