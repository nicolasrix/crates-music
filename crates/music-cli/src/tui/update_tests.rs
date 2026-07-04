//! Reducer tests. No terminal, no HTTP, no audio device — App is built with
//! `player: None` and effects are asserted as *descriptions*, never run.

use bytes::Bytes;
use music_core::{Track, TrackId};
use music_player::PlayerEvent;
use music_subsonic::AlbumWithSongs;

use super::super::msg::{Effect, Msg, StationError};
use super::super::state::{App, LibraryPane, Loadable, Overlay, Rating, Section, to_queued};
use super::update;

fn app() -> App {
    App::new(None, false)
}

fn track(id: &str, title: &str) -> Track {
    Track {
        id: TrackId::from(id.to_owned()),
        title: title.to_owned(),
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

fn album_with_songs(album_id: &str, track_ids: &[&str]) -> AlbumWithSongs {
    AlbumWithSongs {
        album: music_core::Album {
            id: music_core::AlbumId::from(album_id.to_owned()),
            name: format!("album-{album_id}"),
            artist_name: None,
            artist_id: None,
            year: None,
            song_count: u32::try_from(track_ids.len()).unwrap_or(0),
            duration_seconds: 0,
            cover_art_id: None,
            play_count: None,
            played_at: None,
        },
        tracks: track_ids
            .iter()
            .map(|id| track(id, &format!("title-{id}")))
            .collect(),
    }
}

/// Prime the app as if an album detail is open with a queue playing from it.
fn playing_app(track_ids: &[&str], start: usize) -> App {
    let mut a = app();
    let album = album_with_songs("al1", track_ids);
    a.queue
        .replace(album.tracks.iter().map(to_queued).collect(), start);
    a
}

#[test]
fn tick_expires_status() {
    let mut a = app();
    a.set_status("hello", false);
    let expires = a.status.as_ref().unwrap().expires_at;
    for _ in 0..expires {
        assert!(update(&mut a, Msg::Tick).is_empty());
    }
    assert!(a.status.is_none(), "status should expire at its deadline");
}

#[test]
fn quit_sets_flag() {
    let mut a = app();
    update(&mut a, Msg::Quit);
    assert!(a.should_quit);
}

#[test]
fn help_toggles() {
    let mut a = app();
    update(&mut a, Msg::ToggleHelp);
    assert_eq!(a.overlay, Overlay::Help);
    update(&mut a, Msg::Back);
    assert_eq!(a.overlay, Overlay::None);
}

#[test]
fn first_library_visit_loads_albums() {
    let mut a = app();
    a.section = Section::Search;
    let fx = update(&mut a, Msg::GoSection(Section::Library));
    assert!(matches!(fx.as_slice(), [Effect::LoadAlbums { .. }]));
    assert!(matches!(a.library.albums, Loadable::Loading));
    // Second visit: cached, no effect.
    a.library.albums = Loadable::Ready(vec![]);
    a.section = Section::Search;
    assert!(update(&mut a, Msg::GoSection(Section::Library)).is_empty());
}

#[test]
fn stale_albums_response_is_dropped() {
    let mut a = app();
    let fx = update(&mut a, Msg::GoSection(Section::Library));
    let Effect::LoadAlbums { generation, .. } = fx[0].clone() else {
        panic!("expected LoadAlbums");
    };
    // A newer reload bumps the generation…
    update(&mut a, Msg::CycleKindNext);
    // …so the old completion must not land.
    update(
        &mut a,
        Msg::AlbumsLoaded {
            generation,
            result: Ok(vec![]),
        },
    );
    assert!(
        matches!(a.library.albums, Loadable::Loading),
        "stale generation must not overwrite Loading state"
    );
}

#[test]
fn stale_search_response_is_dropped() {
    let mut a = app();
    a.section = Section::Search;
    a.search.generation = 5;
    a.search.results = Loadable::Loading;
    update(
        &mut a,
        Msg::SearchDone {
            generation: 4,
            result: Ok(music_subsonic::SearchResult3::default()),
        },
    );
    assert!(matches!(a.search.results, Loadable::Loading));
    update(
        &mut a,
        Msg::SearchDone {
            generation: 5,
            result: Ok(music_subsonic::SearchResult3::default()),
        },
    );
    assert!(matches!(a.search.results, Loadable::Ready(_)));
}

#[test]
fn activate_album_detail_plays_from_selected_row() {
    let mut a = app();
    a.library.pane = LibraryPane::AlbumDetail;
    a.library.open_album = Loadable::Ready(album_with_songs("al1", &["t1", "t2", "t3"]));
    a.library.tracks_table.select(Some(1));
    let fx = update(&mut a, Msg::Activate);
    assert_eq!(a.queue.len(), 3);
    assert_eq!(a.queue.current().unwrap().id, "t2");
    assert!(matches!(
        fx.as_slice(),
        [Effect::ResolveAudio { queue_index: 1, .. }]
    ));
    assert_eq!(a.pending_load, Some(1));
}

#[test]
fn audio_ready_loads_and_prefetches_next() {
    let mut a = playing_app(&["t1", "t2"], 0);
    a.pending_load = Some(0);
    let fx = update(
        &mut a,
        Msg::AudioReady {
            queue_index: 0,
            track_id: "t1".into(),
            bytes: Bytes::from_static(b"xx"),
        },
    );
    assert_eq!(a.pending_load, None);
    assert!(matches!(
        fx.as_slice(),
        [Effect::PrefetchAudio { track_id }] if track_id == "t2"
    ));
}

#[test]
fn stale_audio_ready_is_dropped() {
    let mut a = playing_app(&["t1", "t2"], 1);
    // Bytes arrive for index 0, but the queue has moved to index 1.
    let fx = update(
        &mut a,
        Msg::AudioReady {
            queue_index: 0,
            track_id: "t1".into(),
            bytes: Bytes::from_static(b"xx"),
        },
    );
    assert!(fx.is_empty());
}

#[test]
fn track_ended_advances_and_uses_prefetch() {
    let mut a = playing_app(&["t1", "t2", "t3"], 0);
    a.prefetched = Some(("t2".into(), Bytes::from_static(b"yy")));
    let fx = update(&mut a, Msg::Player(PlayerEvent::TrackEnded));
    assert_eq!(a.queue.current().unwrap().id, "t2");
    assert!(a.prefetched.is_none(), "prefetch consumed");
    // The prefetch hit means no ResolveAudio — straight to prefetching t3.
    assert!(matches!(
        fx.as_slice(),
        [Effect::PrefetchAudio { track_id }] if track_id == "t3"
    ));
}

#[test]
fn track_ended_without_prefetch_resolves() {
    let mut a = playing_app(&["t1", "t2"], 0);
    let fx = update(&mut a, Msg::Player(PlayerEvent::TrackEnded));
    assert!(matches!(
        fx.as_slice(),
        [Effect::ResolveAudio { queue_index: 1, .. }]
    ));
}

#[test]
fn duplicate_track_ended_is_idempotent() {
    let mut a = playing_app(&["t1"], 0);
    let fx1 = update(&mut a, Msg::Player(PlayerEvent::TrackEnded));
    assert!(fx1.is_empty(), "end of queue: no further effects");
    assert!(a.queue.current().is_none());
    let fx2 = update(&mut a, Msg::Player(PlayerEvent::TrackEnded));
    assert!(fx2.is_empty(), "duplicate TrackEnded must not re-advance");
}

#[test]
fn audio_failed_skips_to_next() {
    let mut a = playing_app(&["t1", "t2"], 0);
    a.pending_load = Some(0);
    let fx = update(
        &mut a,
        Msg::AudioFailed {
            queue_index: 0,
            track_id: "t1".into(),
            error: "boom".into(),
        },
    );
    assert_eq!(a.queue.current().unwrap().id, "t2");
    assert!(matches!(
        fx.as_slice(),
        [Effect::ResolveAudio { queue_index: 1, .. }]
    ));
    assert!(a.status.as_ref().is_some_and(|s| s.is_error));
}

#[test]
fn prefetch_ready_kept_only_if_still_next_up() {
    let mut a = playing_app(&["t1", "t2"], 0);
    update(
        &mut a,
        Msg::PrefetchReady {
            track_id: "t2".into(),
            bytes: Bytes::from_static(b"yy"),
        },
    );
    assert!(a.prefetched.is_some());
    // A prefetch for a track that is no longer next is discarded.
    let mut b = playing_app(&["t1", "t2"], 1);
    update(
        &mut b,
        Msg::PrefetchReady {
            track_id: "t1".into(),
            bytes: Bytes::from_static(b"yy"),
        },
    );
    assert!(b.prefetched.is_none());
}

#[test]
fn rating_applies_optimistically_and_rolls_back_on_error() {
    let mut a = playing_app(&["t1"], 0);
    a.section = Section::Queue;
    a.queue_table.select(Some(0));
    let fx = update(&mut a, Msg::Rate(Some(Rating::Like)));
    assert_eq!(a.ratings.get("t1"), Some(&Rating::Like));
    let Effect::SetRating { id, previous, .. } = fx[0].clone() else {
        panic!("expected SetRating");
    };
    assert_eq!(id, "t1");
    assert_eq!(previous, None);
    // Server rejects → rollback to neutral.
    update(
        &mut a,
        Msg::RatingSet {
            id,
            previous,
            result: Err("403".into()),
        },
    );
    assert!(!a.ratings.contains_key("t1"));
    assert!(a.status.as_ref().is_some_and(|s| s.is_error));
}

#[test]
fn queue_remove_of_playing_track_starts_next() {
    let mut a = playing_app(&["t1", "t2"], 0);
    a.section = Section::Queue;
    a.queue_table.select(Some(0));
    let fx = update(&mut a, Msg::QueueRemoveSelected);
    assert_eq!(a.queue.len(), 1);
    assert_eq!(a.queue.current().unwrap().id, "t2");
    assert!(matches!(fx.as_slice(), [Effect::ResolveAudio { .. }]));
}

#[test]
fn queue_clear_resets_transients() {
    let mut a = playing_app(&["t1", "t2"], 0);
    a.prefetched = Some(("t2".into(), Bytes::from_static(b"yy")));
    a.pending_load = Some(0);
    update(&mut a, Msg::QueueClear);
    assert!(a.queue.is_empty());
    assert!(a.prefetched.is_none());
    assert!(a.pending_load.is_none());
}

#[test]
fn transport_toggle_restarts_finished_queue() {
    let mut a = playing_app(&["t1"], 0);
    // Finish the queue.
    update(&mut a, Msg::Player(PlayerEvent::TrackEnded));
    assert!(a.queue.current().is_none());
    // Space: jump back to the top and resolve.
    let fx = update(&mut a, Msg::TransportToggle);
    assert_eq!(a.queue.current().unwrap().id, "t1");
    assert!(matches!(fx.as_slice(), [Effect::ResolveAudio { .. }]));
}

#[test]
fn no_audio_device_blocks_playback_with_status() {
    let mut a = App::new(None, true);
    let album = album_with_songs("al1", &["t1"]);
    a.queue
        .replace(album.tracks.iter().map(to_queued).collect(), 0);
    let fx = update(&mut a, Msg::TransportToggle);
    assert!(fx.is_empty());
    assert!(a.status.as_ref().is_some_and(|s| s.is_error));
}

#[test]
fn submit_search_bumps_generation_and_unfocuses() {
    let mut a = app();
    a.section = Section::Search;
    a.search.focused = true;
    for c in "beatels".chars() {
        update(&mut a, Msg::Input(super::super::msg::InputMsg::Char(c)));
    }
    let fx = update(&mut a, Msg::SubmitInput);
    assert!(!a.search.focused);
    assert!(matches!(a.search.results, Loadable::Loading));
    assert!(matches!(
        fx.as_slice(),
        [Effect::Search { generation: 1, query }] if query == "beatels"
    ));
}

#[test]
fn empty_search_submit_is_noop() {
    let mut a = app();
    a.section = Section::Search;
    a.search.focused = true;
    assert!(update(&mut a, Msg::SubmitInput).is_empty());
    assert!(a.search.focused, "stays focused for typing");
}

#[test]
fn station_unavailable_renders_friendly_failure() {
    let mut a = app();
    a.section = Section::Stations;
    a.stations.generation = 1;
    a.stations.results = Loadable::Loading;
    update(
        &mut a,
        Msg::StationDone {
            generation: 1,
            result: Err(StationError::Unavailable),
        },
    );
    let Loadable::Failed(text) = &a.stations.results else {
        panic!("expected Failed");
    };
    assert!(text.contains("station unavailable"), "{text}");
}

#[test]
fn recommend_done_enqueues() {
    let mut a = playing_app(&["t1"], 0);
    update(
        &mut a,
        Msg::RecommendDone {
            result: Ok(vec![track("r1", "rec one"), track("r2", "rec two")]),
        },
    );
    assert_eq!(a.queue.len(), 3);
    assert_eq!(a.queue.items()[1].id, "r1");
}

#[test]
fn recommend_needs_a_seed() {
    let mut a = app();
    let fx = update(&mut a, Msg::RecommendFromNowPlaying);
    assert!(fx.is_empty());
    // With a queue current, the seed is taken from it.
    let mut b = playing_app(&["t1"], 0);
    let fx = update(&mut b, Msg::RecommendFromNowPlaying);
    assert!(matches!(
        fx.as_slice(),
        [Effect::RecommendNext { seed, .. }] if seed == "t1"
    ));
}

#[test]
fn nav_clamps_to_list_bounds() {
    let mut a = app();
    a.library.albums = Loadable::Ready(vec![
        album_with_songs("a", &[]).album,
        album_with_songs("b", &[]).album,
    ]);
    a.library.albums_table.select(Some(0));
    update(&mut a, Msg::NavUp);
    assert_eq!(a.library.albums_table.selected(), Some(0));
    update(&mut a, Msg::NavDown);
    update(&mut a, Msg::NavDown);
    update(&mut a, Msg::NavDown);
    assert_eq!(a.library.albums_table.selected(), Some(1));
    update(&mut a, Msg::NavTop);
    assert_eq!(a.library.albums_table.selected(), Some(0));
}
