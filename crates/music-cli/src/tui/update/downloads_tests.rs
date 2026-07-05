//! Downloads / offline reducer tests. No cache, no HTTP — every effect is
//! asserted as a *description*; the pin/unpin/evict I/O lives behind the
//! `Effect` boundary and is never run here.

use music_cache::AudioCacheStats;
use music_core::{Track, TrackId};

use super::super::msg::{Effect, Msg};
use super::super::state::{
    App, LibraryPane, Loadable, PinnedRow, PlaylistDetailState, PlaylistsPane, Section, to_queued,
};
use super::update;
use crate::api::PlaylistSummary;

fn app() -> App {
    App::new(None, false, true)
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

fn stats() -> AudioCacheStats {
    AudioCacheStats {
        regular_count: 3,
        regular_bytes: 300,
        regular_budget_bytes: 1000,
        pinned_count: 2,
        pinned_bytes: 200,
        pinned_budget_bytes: 1000,
    }
}

/// A pinned row, hydrated (`with_meta = true`) or id-only (offline).
fn pinned_row(id: &str, with_meta: bool) -> PinnedRow {
    PinnedRow {
        track_id: id.to_owned(),
        bytes: 100,
        track: with_meta.then(|| track(id, &format!("title-{id}"))),
    }
}

/// App sitting in the Downloads section with a loaded, hydrated pinned table.
fn downloads_app(ids: &[&str]) -> App {
    let mut a = app();
    a.section = Section::Downloads;
    a.downloads.stats = Loadable::Ready(stats());
    a.downloads.pinned = Loadable::Ready(ids.iter().map(|i| pinned_row(i, true)).collect());
    a.downloads.table.select(Some(0));
    a
}

#[test]
fn go_section_downloads_loads() {
    let mut a = app();
    let effects = update(&mut a, Msg::GoSection(Section::Downloads));
    assert_eq!(effects, vec![Effect::LoadDownloads]);
    assert!(matches!(a.downloads.stats, Loadable::Loading));
    assert!(matches!(a.downloads.pinned, Loadable::Loading));
}

#[test]
fn downloads_loaded_populates_and_selects() {
    let mut a = app();
    a.section = Section::Downloads;
    let rows = vec![pinned_row("t1", true), pinned_row("t2", false)];
    let effects = update(
        &mut a,
        Msg::DownloadsLoaded {
            stats: Ok(stats()),
            pinned: Ok(rows),
        },
    );
    assert!(effects.is_empty());
    assert!(matches!(a.downloads.stats, Loadable::Ready(_)));
    assert_eq!(super::loaded_len(&a.downloads.pinned), 2);
    assert_eq!(a.downloads.table.selected(), Some(0));
}

#[test]
fn downloads_loaded_offline_stats_fail_pinned_ok() {
    // Hydration offline still yields id-only rows (pinned Ok), while a local
    // stats read that failed surfaces as Failed — the two halves are independent.
    let mut a = app();
    a.section = Section::Downloads;
    update(
        &mut a,
        Msg::DownloadsLoaded {
            stats: Err("sqlite locked".to_owned()),
            pinned: Ok(vec![pinned_row("t1", false)]),
        },
    );
    assert!(matches!(a.downloads.stats, Loadable::Failed(_)));
    assert_eq!(super::loaded_len(&a.downloads.pinned), 1);
}

#[test]
fn save_offline_targets_selected_pinned_row() {
    let mut a = downloads_app(&["t1", "t2"]);
    a.downloads.table.select(Some(1));
    let effects = update(&mut a, Msg::SaveOffline);
    assert_eq!(
        effects,
        vec![Effect::PinToggle {
            track_id: "t2".to_owned(),
            title: "title-t2".to_owned(),
        }]
    );
}

#[test]
fn save_offline_targets_contextual_queue_row() {
    // Outside Downloads, `d` uses the contextual track row (here, the queue).
    let mut a = app();
    a.section = Section::Queue;
    a.queue.replace(vec![to_queued(&track("q1", "Queued"))], 0);
    a.queue_table.select(Some(0));
    let effects = update(&mut a, Msg::SaveOffline);
    assert_eq!(
        effects,
        vec![Effect::PinToggle {
            track_id: "q1".to_owned(),
            title: "Queued".to_owned(),
        }]
    );
}

#[test]
fn save_offline_no_target_is_noop() {
    let mut a = app();
    a.section = Section::Downloads;
    a.downloads.pinned = Loadable::Ready(vec![]); // nothing selected/present
    a.downloads.table.select(None);
    assert!(update(&mut a, Msg::SaveOffline).is_empty());
    assert!(a.status.is_some());
}

#[test]
fn bulk_download_album_pins_all_tracks() {
    let mut a = app();
    a.section = Section::Library;
    a.library.pane = LibraryPane::AlbumDetail;
    a.library.open_album = Loadable::Ready(music_subsonic::AlbumWithSongs {
        album: music_core::Album {
            id: music_core::AlbumId::from("al1".to_owned()),
            name: "Kind of Blue".to_owned(),
            artist_name: None,
            artist_id: None,
            year: None,
            song_count: 2,
            duration_seconds: 0,
            cover_art_id: None,
            play_count: None,
            played_at: None,
        },
        tracks: vec![track("a1", "So What"), track("a2", "Freddie Freeloader")],
    });
    let effects = update(&mut a, Msg::BulkDownload);
    assert_eq!(
        effects,
        vec![Effect::PinBulk {
            track_ids: vec!["a1".to_owned(), "a2".to_owned()],
            label: "download Kind of Blue".to_owned(),
        }]
    );
}

#[test]
fn bulk_download_playlist_pins_raw_ids() {
    let mut a = app();
    a.section = Section::Playlists;
    a.playlists.pane = PlaylistsPane::Detail;
    a.playlists.open = Loadable::Ready(PlaylistDetailState {
        summary: PlaylistSummary {
            id: "p1".to_owned(),
            name: "Roadtrip".to_owned(),
            visibility: "private".to_owned(),
            owned: true,
            song_count: 2,
        },
        tracks: vec![track("x1", "one")],
        track_ids: vec!["x1".to_owned(), "x2".to_owned()],
    });
    let effects = update(&mut a, Msg::BulkDownload);
    assert_eq!(
        effects,
        vec![Effect::PinBulk {
            track_ids: vec!["x1".to_owned(), "x2".to_owned()],
            label: "download Roadtrip".to_owned(),
        }]
    );
}

#[test]
fn bulk_download_in_downloads_warms_from_liked() {
    let mut a = downloads_app(&["t1"]);
    assert_eq!(update(&mut a, Msg::BulkDownload), vec![Effect::WarmLiked]);
}

#[test]
fn bulk_download_elsewhere_is_noop() {
    let mut a = app(); // Library browse
    assert!(update(&mut a, Msg::BulkDownload).is_empty());
    assert!(a.status.is_some());
}

#[test]
fn evict_emits_effect() {
    let mut a = downloads_app(&["t1"]);
    assert_eq!(update(&mut a, Msg::EvictCache), vec![Effect::EvictCache]);
}

#[test]
fn pin_done_reloads_only_in_downloads() {
    // In the Downloads section a completed pin reloads the view…
    let mut a = downloads_app(&["t1"]);
    let effects = update(
        &mut a,
        Msg::PinDone {
            note: "saved".to_owned(),
            is_error: false,
        },
    );
    assert_eq!(effects, vec![Effect::LoadDownloads]);
    assert_eq!(a.status.as_ref().unwrap().text, "saved");

    // …but from another section it only surfaces the status.
    let mut b = app();
    b.section = Section::Library;
    let effects = update(
        &mut b,
        Msg::PinDone {
            note: "saved X for offline".to_owned(),
            is_error: false,
        },
    );
    assert!(effects.is_empty());
    assert_eq!(b.status.as_ref().unwrap().text, "saved X for offline");
}

#[test]
fn activate_plays_pinned_row_offline_capable() {
    // A row with no hydrated metadata still queues + plays (keyed on id).
    let mut a = app();
    a.section = Section::Downloads;
    a.downloads.pinned = Loadable::Ready(vec![pinned_row("t1", false), pinned_row("t2", false)]);
    a.downloads.table.select(Some(1));
    let effects = update(&mut a, Msg::Activate);
    assert_eq!(a.queue.len(), 2);
    assert_eq!(a.queue.current_index(), Some(1));
    // Offline/local mode resolves the current track's audio.
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::ResolveAudio { track_id, .. } if track_id == "t2"
    )));
}

#[test]
fn nav_moves_pinned_selection() {
    let mut a = downloads_app(&["t1", "t2", "t3"]);
    update(&mut a, Msg::NavDown);
    assert_eq!(a.downloads.table.selected(), Some(1));
    update(&mut a, Msg::NavBottom);
    assert_eq!(a.downloads.table.selected(), Some(2));
}
