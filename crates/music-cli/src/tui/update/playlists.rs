//! Playlists section reducer: the list/detail/suggestions panes, the two
//! modal overlays (add-to-playlist picker, create/rename text prompt), and
//! the effect completions that feed them.
//!
//! Writes are optimistic where the web is (remove-track, add-from-suggestion)
//! and echo-driven where correctness needs the server (create/rename/delete
//! reload the list; a failed optimistic remove resyncs by reopening detail).
//! Playback gestures funnel through [`playback`], so a playlist plays into
//! the sync room exactly like any other track list when the room is online.

use rand::seq::SliceRandom;
use ratatui::widgets::TableState;

use crate::api::PlaylistSummary;
use crate::tui::msg::{Effect, StationError};
use crate::tui::state::{
    App, Loadable, Overlay, PickerState, PlaylistDetailState, PlaylistsPane, PromptPurpose,
    TextPrompt, to_queued,
};
use crate::tui::widgets::input::InputField;

use super::browse;
use super::playback;
use super::{loadable_from, loaded_len, select_first};

/// How many 250 ms ticks a pending delete stays armed (~3 s) before a second
/// `X` no longer confirms it.
const DELETE_CONFIRM_TICKS: u64 = 12;

// ── navigation plumbing ──────────────────────────────────────────────────

/// The (row count, table) the cursor keys act on, by pane.
pub(super) fn focused_list(app: &mut App) -> (usize, &mut TableState) {
    match app.playlists.pane {
        PlaylistsPane::List => (
            loaded_len(&app.playlists.list),
            &mut app.playlists.list_table,
        ),
        PlaylistsPane::Detail => (
            app.playlists.open.ready().map_or(0, |p| p.tracks.len()),
            &mut app.playlists.detail_table,
        ),
        PlaylistsPane::Suggestions => (
            loaded_len(&app.playlists.suggestions),
            &mut app.playlists.suggest_table,
        ),
    }
}

/// `esc` pops one pane level: Suggestions → Detail → List.
pub(super) fn back(app: &mut App) {
    app.playlists.pane = match app.playlists.pane {
        PlaylistsPane::Suggestions => PlaylistsPane::Detail,
        PlaylistsPane::Detail | PlaylistsPane::List => PlaylistsPane::List,
    };
}

/// The selected row's track, for rating / add-to-playlist / play-next.
pub(super) fn selected_track(app: &App) -> Option<music_core::Track> {
    match app.playlists.pane {
        PlaylistsPane::Detail => {
            let sel = app.playlists.detail_table.selected()?;
            app.playlists.open.ready()?.tracks.get(sel).cloned()
        }
        PlaylistsPane::Suggestions => {
            let sel = app.playlists.suggest_table.selected()?;
            app.playlists.suggestions.ready()?.get(sel).cloned()
        }
        PlaylistsPane::List => None,
    }
}

// ── list / detail loads ──────────────────────────────────────────────────

/// Reload the playlist list (bumps the generation so a stale response can't
/// overwrite a newer one).
pub(super) fn reload_list(app: &mut App) -> Vec<Effect> {
    app.playlists.generation += 1;
    app.playlists.list = Loadable::Loading;
    vec![Effect::LoadPlaylists {
        generation: app.playlists.generation,
    }]
}

fn open_detail(app: &mut App, id: String) -> Vec<Effect> {
    app.playlists.pane = PlaylistsPane::Detail;
    app.playlists.open = Loadable::Loading;
    app.playlists.open_id = Some(id.clone());
    // A different playlist's suggestions are stale — reset them.
    app.playlists.suggestions = Loadable::Idle;
    app.playlists.detail_table.select(None);
    vec![Effect::OpenPlaylist { id }]
}

// ── activate / enqueue ───────────────────────────────────────────────────

pub(super) fn activate(app: &mut App) -> Vec<Effect> {
    match app.playlists.pane {
        PlaylistsPane::List => {
            let Some(sel) = app.playlists.list_table.selected() else {
                return vec![];
            };
            let Some(id) = app
                .playlists
                .list
                .ready()
                .and_then(|ps| ps.get(sel))
                .map(|p| p.id.clone())
            else {
                return vec![];
            };
            open_detail(app, id)
        }
        PlaylistsPane::Detail => {
            let Some(sel) = app.playlists.detail_table.selected() else {
                return vec![];
            };
            let Some(open) = app.playlists.open.ready() else {
                return vec![];
            };
            let queued: Vec<_> = open.tracks.iter().map(to_queued).collect();
            playback::play_new_queue(app, queued, sel)
        }
        PlaylistsPane::Suggestions => add_selected_suggestion(app),
    }
}

pub(super) fn enqueue_selected(app: &mut App) -> Vec<Effect> {
    let Some(track) = selected_track(app) else {
        return vec![];
    };
    app.set_status(format!("queued {}", track.title), false);
    playback::enqueue_tracks(app, vec![to_queued(&track)], false)
}

// ── shuffle / suggest ────────────────────────────────────────────────────

/// `s` — shuffle-play the open playlist from a random order.
pub(super) fn shuffle_play(app: &mut App) -> Vec<Effect> {
    if app.playlists.pane == PlaylistsPane::List {
        app.set_status("open a playlist first", false);
        return vec![];
    }
    let Some(open) = app.playlists.open.ready() else {
        return vec![];
    };
    let mut queued: Vec<_> = open.tracks.iter().map(to_queued).collect();
    if queued.is_empty() {
        app.set_status("playlist has no playable tracks", false);
        return vec![];
    }
    queued.shuffle(&mut rand::thread_rng());
    app.set_status("shuffling playlist", false);
    playback::play_new_queue(app, queued, 0)
}

/// `m` — ask the recommender for more tracks like this playlist's.
pub(super) fn suggest(app: &mut App) -> Vec<Effect> {
    let Some(open) = app.playlists.open.ready() else {
        app.set_status("open a playlist first", false);
        return vec![];
    };
    let seeds = open.track_ids.clone();
    let Some(playlist_id) = app.playlists.open_id.clone() else {
        return vec![];
    };
    if seeds.is_empty() {
        app.set_status("playlist is empty — nothing to suggest from", false);
        return vec![];
    }
    app.playlists.pane = PlaylistsPane::Suggestions;
    app.playlists.suggestions = Loadable::Loading;
    app.playlists.suggest_table.select(None);
    vec![Effect::PlaylistSuggest { playlist_id, seeds }]
}

// ── rename / delete / remove-track ───────────────────────────────────────

/// `R` — open the rename prompt for the open playlist.
pub(super) fn rename_prompt(app: &mut App) -> Vec<Effect> {
    let Some(open) = app.playlists.open.ready() else {
        app.set_status("open a playlist to rename it", false);
        return vec![];
    };
    if !open.summary.owned {
        app.set_status("can't rename a playlist you don't own", false);
        return vec![];
    }
    let id = open.summary.id.clone();
    let mut input = InputField::default();
    input.set_value(&open.summary.name);
    app.text_prompt = Some(TextPrompt {
        purpose: PromptPurpose::RenamePlaylist { id },
        title: "Rename playlist".to_owned(),
        input,
    });
    app.overlay = Overlay::TextPrompt;
    vec![]
}

/// `X` — delete the open playlist, two-step (a second `X` within the window
/// confirms). The list refreshes when the delete completes.
pub(super) fn delete(app: &mut App) -> Vec<Effect> {
    let Some(open) = app.playlists.open.ready() else {
        app.set_status("open a playlist to delete it", false);
        return vec![];
    };
    if !open.summary.owned {
        app.set_status("can't delete a playlist you don't own", false);
        return vec![];
    }
    let id = open.summary.id.clone();
    let name = open.summary.name.clone();

    let armed = app.playlists.pending_delete.as_deref() == Some(id.as_str())
        && app.tick < app.playlists.delete_deadline;
    if armed {
        app.playlists.pending_delete = None;
        // Optimistically drop back to the list; the reload confirms.
        app.playlists.pane = PlaylistsPane::List;
        app.playlists.open = Loadable::Idle;
        app.playlists.open_id = None;
        app.set_status(format!("deleting {name}…"), false);
        vec![Effect::PlaylistDelete { id }]
    } else {
        app.playlists.pending_delete = Some(id);
        app.playlists.delete_deadline = app.tick + DELETE_CONFIRM_TICKS;
        app.set_status(format!("press X again to delete {name}"), false);
        vec![]
    }
}

/// `x` — remove the selected track from the open playlist. Optimistic: the
/// row disappears immediately; a failed write reopens the detail to resync.
pub(super) fn remove_track(app: &mut App) -> Vec<Effect> {
    let Some(sel) = app.playlists.detail_table.selected() else {
        return vec![];
    };
    let Some(open) = app.playlists.open.ready() else {
        return vec![];
    };
    if !open.summary.owned {
        app.set_status("can't edit a playlist you don't own", false);
        return vec![];
    }
    let Some(track_id) = open.tracks.get(sel).map(|t| t.id.as_str().to_owned()) else {
        return vec![];
    };
    let id = open.summary.id.clone();
    // Remove *every* occurrence of the id (matches the web), replacing
    // against the raw stored ids so an unhydrated id isn't lost.
    let next_ids: Vec<String> = open
        .track_ids
        .iter()
        .filter(|t| **t != track_id)
        .cloned()
        .collect();

    // Optimistic local update.
    if let Loadable::Ready(open) = &mut app.playlists.open {
        open.tracks.retain(|t| t.id.as_str() != track_id);
        open.track_ids.clone_from(&next_ids);
        open.summary.song_count = u32::try_from(next_ids.len()).unwrap_or(open.summary.song_count);
    }
    let len = app.playlists.open.ready().map_or(0, |p| p.tracks.len());
    app.playlists
        .detail_table
        .select((len > 0).then(|| sel.min(len - 1)));
    app.set_status("removed track from playlist", false);
    vec![Effect::PlaylistSetTracks {
        id,
        track_ids: next_ids,
    }]
}

// ── add-to-playlist picker ───────────────────────────────────────────────

/// `a` — open the picker for the contextually-selected track.
pub(super) fn open_picker(app: &mut App) -> Vec<Effect> {
    let Some((track_id, track_title)) = browse::add_target(app) else {
        app.set_status("no track selected to add", false);
        return vec![];
    };
    let mut table = TableState::default();
    table.select(Some(0));
    app.picker = Some(PickerState {
        track_id,
        track_title,
        table,
    });
    app.overlay = Overlay::PlaylistPicker;
    // Make sure the picker has fresh playlist rows to choose from.
    if app.playlists.list.ready().is_none() {
        reload_list(app)
    } else {
        vec![]
    }
}

/// Owned playlists — the only ones the caller can add to (the gateway 403s a
/// write to someone else's).
fn owned_playlists(app: &App) -> Vec<&PlaylistSummary> {
    app.playlists
        .list
        .ready()
        .map(|ps| ps.iter().filter(|p| p.owned).collect())
        .unwrap_or_default()
}

/// Selectable picker rows = owned playlists + one synthetic "new playlist".
fn picker_row_count(app: &App) -> usize {
    owned_playlists(app).len() + 1
}

pub(super) fn picker_move(app: &mut App, delta: i8) -> Vec<Effect> {
    let rows = picker_row_count(app);
    if let Some(picker) = &mut app.picker {
        let cur = i64::try_from(picker.table.selected().unwrap_or(0)).unwrap_or(0);
        let max = i64::try_from(rows.saturating_sub(1)).unwrap_or(0);
        let next = (cur + i64::from(delta)).clamp(0, max);
        picker.table.select(Some(usize::try_from(next).unwrap_or(0)));
    }
    vec![]
}

pub(super) fn picker_activate(app: &mut App) -> Vec<Effect> {
    let Some(picker) = &app.picker else {
        return vec![];
    };
    let sel = picker.table.selected().unwrap_or(0);
    let owned: Vec<(String, String)> = owned_playlists(app)
        .iter()
        .map(|p| (p.id.clone(), p.name.clone()))
        .collect();

    if let Some((id, name)) = owned.get(sel) {
        // Add to an existing playlist.
        let track_id = picker.track_id.clone();
        let title = picker.track_title.clone();
        close_picker(app);
        app.set_status(format!("added {title} to {name}"), false);
        vec![Effect::PlaylistAddTrack {
            id: id.clone(),
            track_id,
        }]
    } else {
        // The "new playlist…" row: switch to the create-then-add prompt.
        let track_id = picker.track_id.clone();
        close_picker(app);
        app.text_prompt = Some(TextPrompt {
            purpose: PromptPurpose::CreatePlaylistThenAdd { track_id },
            title: "New playlist".to_owned(),
            input: InputField::default(),
        });
        app.overlay = Overlay::TextPrompt;
        vec![]
    }
}

pub(super) fn picker_close(app: &mut App) -> Vec<Effect> {
    close_picker(app);
    vec![]
}

fn close_picker(app: &mut App) {
    app.picker = None;
    app.overlay = Overlay::None;
}

// ── create / rename text prompt ──────────────────────────────────────────

/// `N` — open the create-playlist prompt (from the list pane).
pub(super) fn new_playlist_prompt(app: &mut App) -> Vec<Effect> {
    app.text_prompt = Some(TextPrompt {
        purpose: PromptPurpose::CreatePlaylist,
        title: "New playlist".to_owned(),
        input: InputField::default(),
    });
    app.overlay = Overlay::TextPrompt;
    vec![]
}

pub(super) fn prompt_input(app: &mut App, im: &crate::tui::msg::InputMsg) -> Vec<Effect> {
    if let Some(prompt) = &mut app.text_prompt {
        prompt.input.apply(im);
    }
    vec![]
}

pub(super) fn prompt_submit(app: &mut App) -> Vec<Effect> {
    let Some(prompt) = &app.text_prompt else {
        return vec![];
    };
    let name = prompt.input.value().trim().to_owned();
    if name.is_empty() {
        // Keep the modal open; an empty name is a no-op, not a submit.
        return vec![];
    }
    let purpose = prompt.purpose.clone();
    close_prompt(app);
    match purpose {
        PromptPurpose::CreatePlaylist => {
            app.set_status(format!("creating {name}…"), false);
            vec![Effect::PlaylistCreate {
                name,
                then_add: None,
            }]
        }
        PromptPurpose::CreatePlaylistThenAdd { track_id } => {
            app.set_status(format!("creating {name}…"), false);
            vec![Effect::PlaylistCreate {
                name,
                then_add: Some(track_id),
            }]
        }
        PromptPurpose::RenamePlaylist { id } => {
            app.set_status(format!("renaming to {name}…"), false);
            vec![Effect::PlaylistRename { id, name }]
        }
    }
}

pub(super) fn prompt_close(app: &mut App) -> Vec<Effect> {
    close_prompt(app);
    vec![]
}

fn close_prompt(app: &mut App) {
    app.text_prompt = None;
    app.overlay = Overlay::None;
}

/// Add the selected suggestion to the open playlist (Suggestions pane
/// `enter`). Optimistic: the suggestion leaves the list and the detail gains
/// the track immediately.
fn add_selected_suggestion(app: &mut App) -> Vec<Effect> {
    let Some(sel) = app.playlists.suggest_table.selected() else {
        return vec![];
    };
    let Some(track) = app.playlists.suggestions.ready().and_then(|s| s.get(sel)).cloned() else {
        return vec![];
    };
    let Some(id) = app.playlists.open_id.clone() else {
        return vec![];
    };
    let track_id = track.id.as_str().to_owned();

    // Drop it from the suggestions list so it can't be added twice.
    if let Loadable::Ready(list) = &mut app.playlists.suggestions {
        list.retain(|t| t.id.as_str() != track_id);
    }
    let len = loaded_len(&app.playlists.suggestions);
    app.playlists
        .suggest_table
        .select((len > 0).then(|| sel.min(len - 1)));

    // Optimistically append to the open detail. `id` came from `open_id`, so
    // the open pane is by construction this same playlist.
    if let Loadable::Ready(open) = &mut app.playlists.open {
        open.track_ids.push(track_id.clone());
        open.summary.song_count =
            u32::try_from(open.track_ids.len()).unwrap_or(open.summary.song_count);
        open.tracks.push(track);
    }
    app.set_status("added suggestion to playlist", false);
    vec![Effect::PlaylistAddTrack { id, track_id }]
}

// ── effect completions ───────────────────────────────────────────────────

pub(super) fn on_loaded(
    app: &mut App,
    generation: u64,
    result: Result<Vec<PlaylistSummary>, String>,
) -> Vec<Effect> {
    if generation == app.playlists.generation {
        app.playlists.list = loadable_from(result);
        select_first(
            &mut app.playlists.list_table,
            loaded_len(&app.playlists.list),
        );
    }
    vec![]
}

pub(super) fn on_opened(
    app: &mut App,
    id: &str,
    result: Result<PlaylistDetailState, String>,
) -> Vec<Effect> {
    // Stale if the user opened a different playlist meanwhile.
    if app.playlists.open_id.as_deref() != Some(id) {
        return vec![];
    }
    app.playlists.open = loadable_from(result);
    let len = app.playlists.open.ready().map_or(0, |p| p.tracks.len());
    select_first(&mut app.playlists.detail_table, len);
    vec![]
}

pub(super) fn on_write_done(
    app: &mut App,
    note: String,
    is_error: bool,
    reload_list: bool,
    reopen_id: Option<String>,
) -> Vec<Effect> {
    app.set_status(note, is_error);
    let mut effects = Vec::new();
    if reload_list {
        effects.extend(reload_list_silent(app));
    }
    if let Some(id) = reopen_id {
        // Reopen the detail to resync (rename changed the name; a failed
        // optimistic remove needs the server's truth back).
        app.playlists.open = Loadable::Loading;
        app.playlists.open_id = Some(id.clone());
        effects.push(Effect::OpenPlaylist { id });
    }
    effects
}

/// Reload the list without disturbing the current pane/status (used as a
/// side effect of a write completion).
fn reload_list_silent(app: &mut App) -> Vec<Effect> {
    app.playlists.generation += 1;
    app.playlists.list = Loadable::Loading;
    vec![Effect::LoadPlaylists {
        generation: app.playlists.generation,
    }]
}

pub(super) fn on_suggestions(
    app: &mut App,
    playlist_id: &str,
    result: Result<Vec<music_core::Track>, StationError>,
) -> Vec<Effect> {
    // Drop if the user moved to another playlist while it was in flight.
    if app.playlists.open_id.as_deref() != Some(playlist_id) {
        return vec![];
    }
    app.playlists.suggestions = match result {
        Ok(tracks) if tracks.is_empty() => {
            Loadable::Failed("no suggestions — the playlist may not be embedded yet".to_owned())
        }
        Ok(tracks) => Loadable::Ready(tracks),
        Err(StationError::Unavailable) => Loadable::Failed(
            "suggestions unavailable — the recommender is warming up or offline".to_owned(),
        ),
        Err(StationError::Other(e)) => Loadable::Failed(e),
    };
    select_first(
        &mut app.playlists.suggest_table,
        loaded_len(&app.playlists.suggestions),
    );
    vec![]
}
