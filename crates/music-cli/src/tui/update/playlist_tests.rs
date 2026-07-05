//! Playlists-section reducer tests. Pure `update(&mut App, Msg)` drives —
//! no terminal, no HTTP. Effects are asserted as descriptions, never run.

use music_core::{Track, TrackId};

use super::super::msg::{Effect, InputMsg, Msg, StationError};
use super::super::state::{App, Loadable, Overlay, PlaylistDetailState, PlaylistsPane, Section};
use super::update;
use crate::api::PlaylistSummary;

fn app() -> App {
    let mut a = App::new(None, false, true);
    a.section = Section::Playlists;
    a
}

fn track(id: &str) -> Track {
    Track {
        id: TrackId::from(id.to_owned()),
        title: format!("title-{id}"),
        album_id: None,
        album_name: None,
        artist_id: None,
        artist_name: None,
        track_number: None,
        disc_number: None,
        duration_seconds: Some(180),
        bit_rate_kbps: None,
        content_type: None,
        suffix: None,
        year: None,
        genre: None,
        play_count: None,
        played_at: None,
    }
}

fn summary(id: &str, name: &str, owned: bool, count: u32) -> PlaylistSummary {
    PlaylistSummary {
        id: id.to_owned(),
        name: name.to_owned(),
        visibility: "private".to_owned(),
        owned,
        song_count: count,
    }
}

fn detail(id: &str, name: &str, owned: bool, track_ids: &[&str]) -> PlaylistDetailState {
    PlaylistDetailState {
        summary: summary(id, name, owned, u32::try_from(track_ids.len()).unwrap_or(0)),
        tracks: track_ids.iter().map(|t| track(t)).collect(),
        track_ids: track_ids.iter().map(|s| (*s).to_owned()).collect(),
    }
}

/// App with a loaded list and one owned playlist open in the detail pane.
fn detail_app(track_ids: &[&str]) -> App {
    let mut a = app();
    a.playlists.list = Loadable::Ready(vec![summary("p1", "Roadtrip", true, 3)]);
    a.playlists.list_table.select(Some(0));
    a.playlists.pane = PlaylistsPane::Detail;
    a.playlists.open = Loadable::Ready(detail("p1", "Roadtrip", true, track_ids));
    a.playlists.open_id = Some("p1".to_owned());
    a.playlists.detail_table.select(Some(0));
    a
}

#[test]
fn first_visit_loads_the_list() {
    let mut a = App::new(None, false, true);
    let effects = update(&mut a, Msg::GoSection(Section::Playlists));
    assert!(matches!(effects.as_slice(), [Effect::LoadPlaylists { generation: 1 }]));
    assert!(matches!(a.playlists.list, Loadable::Loading));
}

#[test]
fn playlists_loaded_populates_and_selects_first() {
    let mut a = app();
    let _ = update(&mut a, Msg::GoSection(Section::Playlists)); // gen -> 1
    let effects = update(
        &mut a,
        Msg::PlaylistsLoaded {
            generation: 1,
            result: Ok(vec![summary("p1", "A", true, 1), summary("p2", "B", false, 2)]),
        },
    );
    assert!(effects.is_empty());
    assert_eq!(a.playlists.list.ready().map(Vec::len), Some(2));
    assert_eq!(a.playlists.list_table.selected(), Some(0));
}

#[test]
fn stale_list_generation_is_dropped() {
    let mut a = app();
    let _ = update(&mut a, Msg::GoSection(Section::Playlists)); // gen -> 1
    // A response stamped with an older generation must not overwrite state.
    update(
        &mut a,
        Msg::PlaylistsLoaded {
            generation: 0,
            result: Ok(vec![summary("x", "X", true, 0)]),
        },
    );
    assert!(matches!(a.playlists.list, Loadable::Loading));
}

#[test]
fn activate_list_opens_detail() {
    let mut a = app();
    a.playlists.list = Loadable::Ready(vec![summary("p1", "A", true, 1)]);
    a.playlists.list_table.select(Some(0));
    let effects = update(&mut a, Msg::Activate);
    assert!(matches!(effects.as_slice(), [Effect::OpenPlaylist { id }] if id == "p1"));
    assert_eq!(a.playlists.pane, PlaylistsPane::Detail);
    assert!(matches!(a.playlists.open, Loadable::Loading));
    assert_eq!(a.playlists.open_id.as_deref(), Some("p1"));
}

#[test]
fn opened_for_wrong_id_is_dropped() {
    let mut a = app();
    a.playlists.open_id = Some("p1".to_owned());
    a.playlists.open = Loadable::Loading;
    update(
        &mut a,
        Msg::PlaylistOpened {
            id: "other".to_owned(),
            result: Ok(detail("other", "O", true, &["t1"])),
        },
    );
    assert!(matches!(a.playlists.open, Loadable::Loading));
}

#[test]
fn activate_detail_plays_from_selected_row() {
    let mut a = detail_app(&["t1", "t2", "t3"]);
    a.playlists.detail_table.select(Some(1));
    let effects = update(&mut a, Msg::Activate);
    // Not online → local queue replaced, current track resolves.
    assert!(effects.iter().any(|e| matches!(e, Effect::ResolveAudio { .. })));
    assert_eq!(a.queue.current().map(|t| t.id.clone()), Some("t2".to_owned()));
}

#[test]
fn remove_track_is_optimistic_and_submits_replace() {
    let mut a = detail_app(&["t1", "t2", "t3"]);
    a.playlists.detail_table.select(Some(1)); // t2
    let effects = update(&mut a, Msg::PlaylistRemoveTrack);
    // Local state lost t2 immediately.
    let open = a.playlists.open.ready().unwrap();
    assert_eq!(open.track_ids, ["t1", "t3"]);
    assert_eq!(open.tracks.len(), 2);
    assert_eq!(open.summary.song_count, 2);
    // And a replace was submitted with the surviving ids.
    assert!(matches!(
        effects.as_slice(),
        [Effect::PlaylistSetTracks { id, track_ids }]
            if id == "p1" && track_ids == &["t1".to_owned(), "t3".to_owned()]
    ));
}

#[test]
fn remove_track_refused_on_unowned_playlist() {
    let mut a = detail_app(&["t1", "t2"]);
    if let Loadable::Ready(open) = &mut a.playlists.open {
        open.summary.owned = false;
    }
    let effects = update(&mut a, Msg::PlaylistRemoveTrack);
    assert!(effects.is_empty());
    // Nothing removed.
    assert_eq!(a.playlists.open.ready().unwrap().track_ids.len(), 2);
}

#[test]
fn delete_is_two_step() {
    let mut a = detail_app(&["t1"]);
    // First press just arms — no effect.
    let first = update(&mut a, Msg::PlaylistDelete);
    assert!(first.is_empty());
    assert_eq!(a.playlists.pending_delete.as_deref(), Some("p1"));
    // Second press within the window confirms.
    let second = update(&mut a, Msg::PlaylistDelete);
    assert!(matches!(second.as_slice(), [Effect::PlaylistDelete { id }] if id == "p1"));
    assert_eq!(a.playlists.pane, PlaylistsPane::List);
    assert!(a.playlists.pending_delete.is_none());
}

#[test]
fn delete_confirmation_lapses_after_the_window() {
    let mut a = detail_app(&["t1"]);
    update(&mut a, Msg::PlaylistDelete); // arm at tick 0
    a.tick = 100; // well past the deadline
    let effects = update(&mut a, Msg::PlaylistDelete);
    // Lapsed → re-arms rather than deleting.
    assert!(effects.is_empty());
    assert_eq!(a.playlists.pending_delete.as_deref(), Some("p1"));
}

#[test]
fn suggest_switches_pane_and_requests_seeds() {
    let mut a = detail_app(&["t1", "t2"]);
    let effects = update(&mut a, Msg::PlaylistSuggest);
    assert_eq!(a.playlists.pane, PlaylistsPane::Suggestions);
    assert!(matches!(a.playlists.suggestions, Loadable::Loading));
    assert!(matches!(
        effects.as_slice(),
        [Effect::PlaylistSuggest { playlist_id, seeds }]
            if playlist_id == "p1" && seeds == &["t1".to_owned(), "t2".to_owned()]
    ));
}

#[test]
fn suggestions_done_populates_when_current() {
    let mut a = detail_app(&["t1"]);
    update(&mut a, Msg::PlaylistSuggest);
    update(
        &mut a,
        Msg::PlaylistSuggestionsDone {
            playlist_id: "p1".to_owned(),
            result: Ok(vec![track("s1"), track("s2")]),
        },
    );
    assert_eq!(a.playlists.suggestions.ready().map(Vec::len), Some(2));
}

#[test]
fn suggestions_unavailable_becomes_a_friendly_failure() {
    let mut a = detail_app(&["t1"]);
    update(&mut a, Msg::PlaylistSuggest);
    update(
        &mut a,
        Msg::PlaylistSuggestionsDone {
            playlist_id: "p1".to_owned(),
            result: Err(StationError::Unavailable),
        },
    );
    assert!(matches!(a.playlists.suggestions, Loadable::Failed(_)));
}

#[test]
fn add_suggestion_is_optimistic() {
    let mut a = detail_app(&["t1"]);
    update(&mut a, Msg::PlaylistSuggest);
    update(
        &mut a,
        Msg::PlaylistSuggestionsDone {
            playlist_id: "p1".to_owned(),
            result: Ok(vec![track("s1"), track("s2")]),
        },
    );
    a.playlists.suggest_table.select(Some(0)); // s1
    let effects = update(&mut a, Msg::Activate);
    // s1 left the suggestions and joined the open detail.
    assert_eq!(a.playlists.suggestions.ready().map(Vec::len), Some(1));
    let open = a.playlists.open.ready().unwrap();
    assert!(open.track_ids.contains(&"s1".to_owned()));
    assert!(matches!(
        effects.as_slice(),
        [Effect::PlaylistAddTrack { id, track_id }] if id == "p1" && track_id == "s1"
    ));
}

#[test]
fn add_to_playlist_picker_opens_for_selected_track() {
    let mut a = detail_app(&["t1", "t2"]);
    a.playlists.list = Loadable::Ready(vec![summary("p1", "Roadtrip", true, 3)]);
    a.playlists.detail_table.select(Some(0)); // t1
    let effects = update(&mut a, Msg::AddToPlaylist);
    assert_eq!(a.overlay, Overlay::PlaylistPicker);
    assert_eq!(a.picker.as_ref().map(|p| p.track_id.clone()), Some("t1".to_owned()));
    // List already loaded → no reload effect.
    assert!(effects.is_empty());
}

#[test]
fn picker_activate_on_owned_playlist_adds_track() {
    let mut a = detail_app(&["t1"]);
    a.playlists.list = Loadable::Ready(vec![summary("p9", "Faves", true, 0)]);
    update(&mut a, Msg::AddToPlaylist); // opens picker for t1, selection 0
    let effects = update(&mut a, Msg::PickerActivate);
    assert_eq!(a.overlay, Overlay::None);
    assert!(a.picker.is_none());
    assert!(matches!(
        effects.as_slice(),
        [Effect::PlaylistAddTrack { id, track_id }] if id == "p9" && track_id == "t1"
    ));
}

#[test]
fn picker_new_row_opens_create_then_add_prompt() {
    let mut a = detail_app(&["t1"]);
    a.playlists.list = Loadable::Ready(vec![summary("p9", "Faves", true, 0)]);
    update(&mut a, Msg::AddToPlaylist);
    // Move past the single owned playlist onto the synthetic "new" row.
    update(&mut a, Msg::PickerMove(1));
    let effects = update(&mut a, Msg::PickerActivate);
    assert!(effects.is_empty());
    assert_eq!(a.overlay, Overlay::TextPrompt);
    assert!(a.picker.is_none());
    assert!(a.text_prompt.is_some());
}

#[test]
fn new_playlist_prompt_submit_creates() {
    let mut a = app();
    update(&mut a, Msg::NewPlaylist);
    assert_eq!(a.overlay, Overlay::TextPrompt);
    for c in "Focus".chars() {
        update(&mut a, Msg::PromptInput(InputMsg::Char(c)));
    }
    let effects = update(&mut a, Msg::PromptSubmit);
    assert_eq!(a.overlay, Overlay::None);
    assert!(matches!(
        effects.as_slice(),
        [Effect::PlaylistCreate { name, then_add: None }] if name == "Focus"
    ));
}

#[test]
fn empty_prompt_submit_is_a_no_op() {
    let mut a = app();
    update(&mut a, Msg::NewPlaylist);
    let effects = update(&mut a, Msg::PromptSubmit);
    // Nothing submitted, modal stays open.
    assert!(effects.is_empty());
    assert_eq!(a.overlay, Overlay::TextPrompt);
}

#[test]
fn write_done_reopens_and_reloads() {
    let mut a = detail_app(&["t1"]);
    let effects = update(
        &mut a,
        Msg::PlaylistWriteDone {
            note: "renamed".to_owned(),
            is_error: false,
            reload_list: true,
            reopen_id: Some("p1".to_owned()),
        },
    );
    // Both a list reload and a detail reopen go out.
    assert!(effects.iter().any(|e| matches!(e, Effect::LoadPlaylists { .. })));
    assert!(effects.iter().any(|e| matches!(e, Effect::OpenPlaylist { id } if id == "p1")));
    assert!(matches!(a.playlists.open, Loadable::Loading));
}

#[test]
fn back_pops_panes_toward_the_list() {
    let mut a = detail_app(&["t1"]);
    a.playlists.pane = PlaylistsPane::Suggestions;
    update(&mut a, Msg::Back);
    assert_eq!(a.playlists.pane, PlaylistsPane::Detail);
    update(&mut a, Msg::Back);
    assert_eq!(a.playlists.pane, PlaylistsPane::List);
}

#[test]
fn add_target_resolves_a_queue_row() {
    // `a` in the queue adds the selected queue item, whose row is a
    // QueuedTrack (not a full Track) — the add-target must still resolve it.
    let mut a = app();
    a.section = Section::Queue;
    a.queue.replace(
        vec![super::super::state::to_queued(&track("q1"))],
        0,
    );
    a.queue_table.select(Some(0));
    a.playlists.list = Loadable::Ready(vec![summary("p1", "A", true, 0)]);
    update(&mut a, Msg::AddToPlaylist);
    assert_eq!(a.picker.as_ref().map(|p| p.track_id.clone()), Some("q1".to_owned()));
}
