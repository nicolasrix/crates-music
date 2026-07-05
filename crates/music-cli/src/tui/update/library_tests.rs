//! Library-section reducer tests: browse modes, artist detail, the album
//! station and similar footer, and liked navigation. Pure `update(&mut App,
//! Msg)` — no terminal, no HTTP; effects are asserted as descriptions.

use music_core::{Album, AlbumId, Artist, ArtistId, Track, TrackId};
use music_subsonic::AlbumWithSongs;

use super::super::msg::{Effect, Msg, StationError};
use super::super::state::{
    App, ArtistDetailState, ArtistRow, LibraryMode, LibraryPane, Loadable, Rating, Section,
    SimilarEntry, SimilarKind,
};
use super::update;

fn app() -> App {
    let mut a = App::new(None, false, true);
    a.section = Section::Library;
    a
}

fn track(id: &str) -> Track {
    Track {
        id: TrackId::from(id.to_owned()),
        title: format!("title-{id}"),
        album_id: Some(AlbumId::from("al1".to_owned())),
        album_name: Some("Album".to_owned()),
        artist_id: Some(ArtistId::from("ar1".to_owned())),
        artist_name: Some("Artist".to_owned()),
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

fn album(id: &str, name: &str) -> Album {
    Album {
        id: AlbumId::from(id.to_owned()),
        name: name.to_owned(),
        artist_name: Some("Artist".to_owned()),
        artist_id: Some(ArtistId::from("ar1".to_owned())),
        year: Some(2020),
        song_count: 3,
        duration_seconds: 600,
        cover_art_id: None,
        play_count: None,
        played_at: None,
    }
}

fn artist(id: &str, name: &str) -> Artist {
    Artist {
        id: ArtistId::from(id.to_owned()),
        name: name.to_owned(),
        album_count: Some(2),
    }
}

fn album_with_songs(id: &str, track_ids: &[&str]) -> AlbumWithSongs {
    AlbumWithSongs {
        album: album(id, "Album"),
        tracks: track_ids.iter().map(|t| track(t)).collect(),
    }
}

/// App sitting on an open album-detail pane with tracks loaded.
fn album_app(track_ids: &[&str]) -> App {
    let mut a = app();
    a.library.pane = LibraryPane::AlbumDetail;
    a.library.open_target = Some("al1".to_owned());
    a.library.open_album = Loadable::Ready(album_with_songs("al1", track_ids));
    a.library.tracks_table.select(Some(0));
    a
}

fn similar(kind: SimilarKind, id: &str, name: &str) -> SimilarEntry {
    SimilarEntry {
        kind,
        id: id.to_owned(),
        name: name.to_owned(),
        artist: None,
    }
}

// ── browse modes ────────────────────────────────────────────────────────

#[test]
fn cycle_mode_advances_and_loads_that_modes_list() {
    let mut a = app();
    // albums → artists
    let fx = update(&mut a, Msg::CycleModeNext);
    assert_eq!(a.library.mode, LibraryMode::Artists);
    assert!(matches!(a.library.artists, Loadable::Loading));
    assert!(fx.iter().any(|e| matches!(e, Effect::LoadArtists { .. })));

    // artists → tracks
    let fx = update(&mut a, Msg::CycleModeNext);
    assert_eq!(a.library.mode, LibraryMode::Tracks);
    assert!(fx.iter().any(|e| matches!(e, Effect::LoadSongs { .. })));

    // tracks → albums (wraps)
    let fx = update(&mut a, Msg::CycleModeNext);
    assert_eq!(a.library.mode, LibraryMode::Albums);
    assert!(fx.iter().any(|e| matches!(e, Effect::LoadAlbums { .. })));
}

#[test]
fn cycle_mode_is_inert_in_a_detail_pane() {
    let mut a = album_app(&["t1"]);
    let fx = update(&mut a, Msg::CycleModeNext);
    // Still on the album, mode unchanged, no reload.
    assert_eq!(a.library.pane, LibraryPane::AlbumDetail);
    assert_eq!(a.library.mode, LibraryMode::Albums);
    assert!(fx.is_empty());
}

#[test]
fn artists_loaded_populates_and_selects_first_with_generation_guard() {
    let mut a = app();
    update(&mut a, Msg::CycleModeNext); // → Artists, generation bumped, Loading
    let gen0 = a.library.generation;

    // A stale response (older generation) is dropped.
    update(
        &mut a,
        Msg::ArtistsLoaded {
            generation: gen0 - 1,
            result: Ok(vec![artist("ar9", "Stale")]),
        },
    );
    assert!(matches!(a.library.artists, Loadable::Loading));

    update(
        &mut a,
        Msg::ArtistsLoaded {
            generation: gen0,
            result: Ok(vec![artist("ar1", "Aphex"), artist("ar2", "Autechre")]),
        },
    );
    assert_eq!(a.library.artists.ready().map(Vec::len), Some(2));
    assert_eq!(a.library.artists_table.selected(), Some(0));
}

#[test]
fn activating_an_artist_row_opens_the_artist_detail() {
    let mut a = app();
    a.library.mode = LibraryMode::Artists;
    a.library.artists = Loadable::Ready(vec![artist("ar1", "Aphex Twin")]);
    a.library.artists_table.select(Some(0));

    let fx = update(&mut a, Msg::Activate);
    assert_eq!(a.library.pane, LibraryPane::ArtistDetail);
    assert_eq!(a.library.artist_target.as_deref(), Some("ar1"));
    assert!(fx.iter().any(
        |e| matches!(e, Effect::OpenArtist { id, name } if id == "ar1" && name == "Aphex Twin")
    ));
}

// ── album detail: similar footer + flat selection ───────────────────────

#[test]
fn opening_an_album_kicks_off_the_similar_footer_with_track_seeds() {
    let mut a = app();
    a.library.pane = LibraryPane::AlbumDetail;
    a.library.open_target = Some("al1".to_owned());

    let fx = update(
        &mut a,
        Msg::AlbumOpened {
            id: AlbumId::from("al1".to_owned()),
            result: Ok(album_with_songs("al1", &["t1", "t2"])),
        },
    );
    assert!(matches!(a.library.album_similar, Loadable::Loading));
    assert!(fx.iter().any(|e| matches!(
        e,
        Effect::LoadAlbumSimilar { album_id, seed_track_ids, .. }
            if album_id == "al1" && seed_track_ids == &["t1".to_owned(), "t2".to_owned()]
    )));
}

#[test]
fn album_similar_footer_is_dropped_for_a_since_closed_album() {
    let mut a = album_app(&["t1"]);
    a.library.album_similar = Loadable::Loading;
    // A footer for a different album than the one now open is ignored.
    update(
        &mut a,
        Msg::AlbumSimilarLoaded {
            album_id: "OTHER".to_owned(),
            result: Ok(vec![similar(SimilarKind::Album, "al2", "Other")]),
        },
    );
    assert!(matches!(a.library.album_similar, Loadable::Loading));
}

#[test]
fn album_detail_selection_spans_tracks_then_similar_and_navigates() {
    let mut a = album_app(&["t1", "t2"]);
    a.library.album_similar =
        Loadable::Ready(vec![similar(SimilarKind::Album, "al2", "Neighbour")]);

    // Two tracks (idx 0,1) then one similar row (idx 2). Move to the footer.
    update(&mut a, Msg::NavBottom);
    assert_eq!(a.library.tracks_table.selected(), Some(2));

    // Enter on the similar-album row opens that album.
    let fx = update(&mut a, Msg::Activate);
    assert_eq!(a.library.open_target.as_deref(), Some("al2"));
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::OpenAlbum { id } if id.as_str() == "al2"))
    );
}

#[test]
fn enter_on_an_album_track_plays_from_that_track() {
    let mut a = album_app(&["t1", "t2", "t3"]);
    a.library.tracks_table.select(Some(1));
    update(&mut a, Msg::Activate);
    // The whole album is queued, starting at the selected track.
    assert_eq!(a.queue.len(), 3);
    assert_eq!(
        a.queue.current().map(|t| t.id.clone()),
        Some("t2".to_owned())
    );
}

// ── album / artist station (S) ──────────────────────────────────────────

#[test]
fn album_station_seeds_from_the_open_album_tracks() {
    let mut a = album_app(&["t1", "t2"]);
    let fx = update(&mut a, Msg::AlbumStation);
    assert!(fx.iter().any(|e| matches!(
        e,
        Effect::AlbumStation { candidate_seeds } if candidate_seeds == &["t1".to_owned(), "t2".to_owned()]
    )));
}

#[test]
fn album_station_done_replaces_the_queue() {
    let mut a = album_app(&["t1"]);
    // Pre-existing queue that the station should replace.
    update(
        &mut a,
        Msg::AlbumStationDone {
            result: Ok(vec![track("s1"), track("s2"), track("s3")]),
        },
    );
    assert_eq!(a.queue.len(), 3);
    assert_eq!(
        a.queue.current().map(|t| t.id.clone()),
        Some("s1".to_owned())
    );
}

#[test]
fn album_station_unavailable_sets_a_status_not_a_crash() {
    let mut a = album_app(&["t1"]);
    let fx = update(
        &mut a,
        Msg::AlbumStationDone {
            result: Err(StationError::Unavailable),
        },
    );
    assert!(fx.is_empty());
    assert!(a.status.as_ref().is_some_and(|s| s.is_error));
}

// ── artist detail ───────────────────────────────────────────────────────

fn artist_detail_app() -> App {
    let mut a = app();
    a.library.pane = LibraryPane::ArtistDetail;
    a.library.artist_target = Some("ar1".to_owned());
    let rows = vec![
        ArtistRow::Album(album("al1", "First")),
        ArtistRow::Album(album("al2", "Second")),
        ArtistRow::Song(track("t1")),
        ArtistRow::Song(track("t2")),
    ];
    a.library.open_artist = Loadable::Ready(ArtistDetailState {
        artist: artist("ar1", "Aphex Twin"),
        rows,
        albums_len: 2,
    });
    a
}

#[test]
fn artist_detail_album_row_opens_the_album_song_row_plays() {
    let mut a = artist_detail_app();
    // Album row (idx 0) → open album.
    a.library.artist_table.select(Some(0));
    let fx = update(&mut a, Msg::Activate);
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::OpenAlbum { id } if id.as_str() == "al1"))
    );

    // Song row (idx 2, first of the two top songs) → play from there.
    let mut a = artist_detail_app();
    a.library.artist_table.select(Some(3)); // second top song
    update(&mut a, Msg::Activate);
    assert_eq!(a.queue.len(), 2); // both top songs queued
    assert_eq!(
        a.queue.current().map(|t| t.id.clone()),
        Some("t2".to_owned())
    );
}

#[test]
fn artist_station_seeds_from_top_songs() {
    let mut a = artist_detail_app();
    let fx = update(&mut a, Msg::AlbumStation);
    assert!(fx.iter().any(|e| matches!(
        e,
        Effect::AlbumStation { candidate_seeds } if candidate_seeds == &["t1".to_owned(), "t2".to_owned()]
    )));
}

#[test]
fn rating_in_artist_detail_defaults_to_the_artist() {
    let mut a = artist_detail_app();
    a.library.artist_table.select(None);
    update(&mut a, Msg::Rate(Some(Rating::Like)));
    // The artist id is the one that got the optimistic like.
    assert_eq!(a.ratings.get("ar1"), Some(&Rating::Like));
}

// ── liked navigation ────────────────────────────────────────────────────

#[test]
fn liked_album_and_artist_rows_navigate_to_their_detail() {
    use super::super::state::LikedEntry;
    let mut a = app();
    a.section = Section::Liked;
    a.liked.entries = Loadable::Ready(vec![
        LikedEntry {
            kind: "album".to_owned(),
            id: "al5".to_owned(),
            rating: Rating::Like,
            track: None,
            label: Some("Loved Album".to_owned()),
        },
        LikedEntry {
            kind: "artist".to_owned(),
            id: "ar5".to_owned(),
            rating: Rating::Like,
            track: None,
            label: Some("Loved Artist".to_owned()),
        },
    ]);

    a.liked.table.select(Some(0));
    let fx = update(&mut a, Msg::Activate);
    assert_eq!(a.library.open_target.as_deref(), Some("al5"));
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::OpenAlbum { id } if id.as_str() == "al5"))
    );

    a.section = Section::Liked;
    a.liked.table.select(Some(1));
    let fx = update(&mut a, Msg::Activate);
    assert_eq!(a.library.artist_target.as_deref(), Some("ar5"));
    assert!(fx.iter().any(
        |e| matches!(e, Effect::OpenArtist { id, name } if id == "ar5" && name == "Loved Artist")
    ));
}

// ── back() / esc navigation (review findings) ────────────────────────────

#[test]
fn esc_from_a_liked_opened_album_returns_to_liked() {
    use super::super::state::LikedEntry;
    let mut a = app();
    a.section = Section::Liked;
    a.liked.entries = Loadable::Ready(vec![LikedEntry {
        kind: "album".to_owned(),
        id: "al5".to_owned(),
        rating: Rating::Like,
        track: None,
        label: Some("Loved Album".to_owned()),
    }]);
    a.liked.table.select(Some(0));

    update(&mut a, Msg::Activate); // → Library / AlbumDetail
    assert_eq!(a.section, Section::Library);
    assert_eq!(a.library.pane, LibraryPane::AlbumDetail);

    // esc backs out to where we came from (Liked), not the Library browse list.
    update(&mut a, Msg::Back);
    assert_eq!(a.section, Section::Liked);
}

#[test]
fn esc_from_nested_artist_album_returns_to_the_artist_then_browse() {
    let mut a = artist_detail_app(); // Library / ArtistDetail
    a.library.artist_table.select(Some(0)); // an album row
    update(&mut a, Msg::Activate); // → AlbumDetail (nested)
    assert_eq!(a.library.pane, LibraryPane::AlbumDetail);

    // First esc returns to the artist detail we navigated from…
    update(&mut a, Msg::Back);
    assert_eq!(a.section, Section::Library);
    assert_eq!(a.library.pane, LibraryPane::ArtistDetail);

    // …and a second esc falls back to the browse list.
    update(&mut a, Msg::Back);
    assert_eq!(a.library.pane, LibraryPane::Browse);
}

#[test]
fn a_failed_browse_list_reloads_on_section_reentry() {
    let mut a = app();
    a.library.mode = LibraryMode::Tracks;
    a.library.songs = Loadable::Failed("boom".to_owned());
    a.section = Section::Queue; // leave Library

    // Returning to Library retries the failed load rather than stranding.
    let fx = update(&mut a, Msg::GoSection(Section::Library));
    assert!(matches!(a.library.songs, Loadable::Loading));
    assert!(fx.iter().any(|e| matches!(e, Effect::LoadSongs { .. })));
}

#[test]
fn album_similar_footer_is_kept_when_it_matches_the_open_album() {
    let mut a = album_app(&["t1"]); // open_album Ready with id "al1"
    a.library.album_similar = Loadable::Loading;
    update(
        &mut a,
        Msg::AlbumSimilarLoaded {
            album_id: "al1".to_owned(),
            result: Ok(vec![similar(SimilarKind::Artist, "ar9", "Neighbour")]),
        },
    );
    assert_eq!(a.library.album_similar.ready().map(Vec::len), Some(1));
}
