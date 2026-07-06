//! Browse-side reducer logic: turning a selected row into playback. The
//! "activate / enqueue / play-next" cluster, split out of the dispatcher so
//! [`super`] stays a thin dispatcher plus navigation plumbing. Each entry
//! point resolves the current section's selection and funnels into a
//! [`playback`] entry point (which itself forks to [`room`] when the sync
//! connection is online).

use crate::tui::msg::Effect;
use crate::tui::state::{App, LibraryPane, SearchBucket, Section, to_queued};

use super::{downloads, library, playback, playlists, room, settings};

pub(super) fn activate(app: &mut App) -> Vec<Effect> {
    match app.section {
        Section::Library => match app.library.pane {
            LibraryPane::Browse => library::browse_activate(app),
            LibraryPane::AlbumDetail => library::album_activate(app),
            LibraryPane::ArtistDetail => library::artist_activate(app),
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
        Section::Playlists => playlists::activate(app),
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
        Section::Liked => activate_liked(app),
        Section::Downloads => downloads::activate(app),
        Section::Settings => settings::activate(app),
        // Diagnostics' Enter is handled upstream (diagnostics::activate) before
        // this dispatch; never reached here.
        Section::Diagnostics => vec![],
    }
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
            library::open_album(app, album.id.clone())
        }
        SearchBucket::Artists => {
            let Some((id, name)) = results
                .artists
                .get(sel)
                .map(|a| (a.id.as_str().to_owned(), a.name.clone()))
            else {
                return vec![];
            };
            library::open_artist(app, id, name)
        }
    }
}

/// Enter on a Liked row: play a liked *track* (starting the whole liked-track
/// list from it), or navigate to a liked *album*/*artist*'s detail pane.
fn activate_liked(app: &mut App) -> Vec<Effect> {
    let Some(sel) = app.liked.table.selected() else {
        return vec![];
    };
    let Some(entries) = app.liked.entries.ready() else {
        return vec![];
    };
    let Some(entry) = entries.get(sel) else {
        return vec![];
    };
    if let Some(track) = entry.track.as_ref() {
        let tracks: Vec<_> = entries
            .iter()
            .filter_map(|e| e.track.as_ref())
            .map(to_queued)
            .collect();
        let start = tracks
            .iter()
            .position(|t| t.id == track.id.as_str())
            .unwrap_or(0);
        return playback::play_new_queue(app, tracks, start);
    }
    // Album / artist rows navigate to their detail pane (using the resolved
    // name when we have it).
    match entry.kind.as_str() {
        "album" => library::open_album(app, music_core::AlbumId::from(entry.id.clone())),
        "artist" => {
            let name = entry.label.clone().unwrap_or_else(|| entry.id.clone());
            library::open_artist(app, entry.id.clone(), name)
        }
        _ => {
            app.set_status("nothing to open for this row", false);
            vec![]
        }
    }
}

pub(super) fn enqueue_selected(app: &mut App) -> Vec<Effect> {
    match app.section {
        Section::Library => match app.library.pane {
            LibraryPane::Browse => library::browse_enqueue(app),
            LibraryPane::AlbumDetail => library::album_enqueue(app),
            LibraryPane::ArtistDetail => library::artist_enqueue(app),
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
        Section::Playlists => playlists::enqueue_selected(app),
        Section::Downloads => downloads::enqueue_selected(app),
        Section::Queue | Section::Settings | Section::Diagnostics => vec![],
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
pub(super) fn play_next_selected(app: &mut App) -> Vec<Effect> {
    if app.section == Section::Queue {
        return playback::queue_move(app, playback::MoveKind::AfterCursor);
    }
    let Some(track) = selected_track(app) else {
        app.set_status("play-next works on track rows", false);
        return vec![];
    };
    app.set_status(format!("playing {} next", track.title), false);
    let queued = to_queued(&track);
    playback::enqueue_tracks(app, vec![queued], true)
}

/// The `(track_id, title)` the add-to-playlist gesture targets in the
/// current context — a superset of [`selected_track`] that also covers the
/// queue (whose rows are `QueuedTrack`s, not full `Track`s).
pub(super) fn add_target(app: &App) -> Option<(String, String)> {
    if app.section == Section::Queue {
        let sel = app.queue_table.selected()?;
        let item = app.queue.items().get(sel)?;
        return Some((item.id.clone(), item.title.clone()));
    }
    let t = selected_track(app)?;
    Some((t.id.as_str().to_owned(), t.title.clone()))
}

/// The selected row's track, in sections that list tracks. Also drives the
/// add-to-playlist gesture, so it must cover every track-listing pane.
pub(super) fn selected_track(app: &App) -> Option<music_core::Track> {
    match app.section {
        Section::Library => match app.library.pane {
            LibraryPane::Browse => library::browse_selected_track(app),
            LibraryPane::AlbumDetail => library::album_selected_track(app),
            LibraryPane::ArtistDetail => library::artist_selected_track(app),
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
        Section::Playlists => playlists::selected_track(app),
        Section::Downloads => downloads::selected_track(app),
        Section::Queue | Section::Settings | Section::Diagnostics => None,
    }
}
