//! Lyric-pane reducer tests. No HTTP: every fetch is asserted as an
//! `Effect` description, and every response is fed in as a `Msg`.

use std::time::Duration;

use crate::api::{LyricLine, LyricsDoc, LyricsOutcome};

use super::super::msg::{Effect, Msg};
use super::super::state::{App, Loadable, Overlay};
use super::update;

fn app() -> App {
    App::new(None, false, true)
}

/// An app with a track loaded in the player snapshot — the only thing the
/// pane reads to decide *which* track it is showing.
fn playing(track_id: &str) -> App {
    let mut app = app();
    app.playback.track_id = Some(track_id.to_owned());
    app
}

fn line(start_ms: i64, text: &str) -> LyricLine {
    LyricLine {
        start_ms,
        text: text.to_owned(),
    }
}

fn synced(track_id: &str, lines: Vec<LyricLine>) -> LyricsOutcome {
    LyricsOutcome::Doc(Box::new(LyricsDoc {
        track_id: track_id.to_owned(),
        source: "lrclib".to_owned(),
        match_kind: Some("exact".to_owned()),
        synced: true,
        instrumental: false,
        lines: Some(lines),
        plain: None,
    }))
}

/// Open the pane on `track_id` with a three-line document already loaded.
fn opened_with_lines(track_id: &str) -> App {
    let mut app = playing(track_id);
    let _ = update(&mut app, Msg::ToggleLyrics);
    let _ = update(
        &mut app,
        Msg::LyricsLoaded {
            track_id: track_id.to_owned(),
            result: Ok(synced(
                track_id,
                vec![line(0, "one"), line(10_000, "two"), line(20_000, "three")],
            )),
        },
    );
    app
}

#[test]
fn toggling_open_fetches_the_now_playing_track() {
    let mut app = playing("t1");
    let effects = update(&mut app, Msg::ToggleLyrics);
    assert_eq!(app.overlay, Overlay::Lyrics);
    assert_eq!(
        effects,
        vec![Effect::LoadLyrics {
            track_id: "t1".to_owned(),
            force: false
        }]
    );
}

#[test]
fn toggling_with_nothing_playing_says_so_instead_of_opening() {
    let mut app = app();
    let effects = update(&mut app, Msg::ToggleLyrics);
    assert_eq!(app.overlay, Overlay::None);
    assert!(effects.is_empty());
    assert!(app.status.is_some(), "the user needs to know why nothing opened");
}

#[test]
fn reopening_the_same_track_does_not_refetch() {
    let mut app = opened_with_lines("t1");
    let _ = update(&mut app, Msg::ToggleLyrics); // close
    assert_eq!(app.overlay, Overlay::None);
    let effects = update(&mut app, Msg::ToggleLyrics); // reopen
    assert!(
        effects.is_empty(),
        "the document is still in hand; refetching would flash a spinner for nothing"
    );
}

#[test]
fn escape_closes_the_pane() {
    let mut app = opened_with_lines("t1");
    let _ = update(&mut app, Msg::Back);
    assert_eq!(app.overlay, Overlay::None);
}

#[test]
fn a_track_change_reloads_while_the_pane_is_open() {
    let mut app = opened_with_lines("t1");
    app.playback.track_id = Some("t2".to_owned());
    let effects = update(&mut app, Msg::Tick);
    assert!(effects.contains(&Effect::LoadLyrics {
        track_id: "t2".to_owned(),
        force: false
    }));
    assert!(app.lyrics.lines.is_empty(), "stale lines must not linger");
}

#[test]
fn a_track_change_does_not_reload_while_the_pane_is_closed() {
    // The pane is the only consumer; fetching lyrics for every track played
    // would send the whole listening history's titles to the provider.
    let mut app = opened_with_lines("t1");
    let _ = update(&mut app, Msg::ToggleLyrics);
    app.playback.track_id = Some("t2".to_owned());
    let effects = update(&mut app, Msg::Tick);
    assert!(!effects.iter().any(|e| matches!(e, Effect::LoadLyrics { .. })));
}

#[test]
fn a_response_for_a_track_we_have_moved_past_is_dropped() {
    let mut app = opened_with_lines("t1");
    app.lyrics.track_id = Some("t2".to_owned());
    let _ = update(
        &mut app,
        Msg::LyricsLoaded {
            track_id: "t1".to_owned(),
            result: Ok(synced("t1", vec![line(0, "stale")])),
        },
    );
    assert_eq!(
        app.lyrics.lines.len(),
        3,
        "a late arrival must not overwrite the current track's document"
    );
}

#[test]
fn lines_are_stored_in_timestamp_order() {
    let mut app = playing("t1");
    let _ = update(&mut app, Msg::ToggleLyrics);
    let _ = update(
        &mut app,
        Msg::LyricsLoaded {
            track_id: "t1".to_owned(),
            result: Ok(synced("t1", vec![line(20_000, "c"), line(0, "a")])),
        },
    );
    assert_eq!(
        app.lyrics.lines.iter().map(|l| l.start_ms).collect::<Vec<_>>(),
        vec![0, 20_000]
    );
}

#[test]
fn an_unsynced_document_stores_no_timed_lines() {
    let mut app = playing("t1");
    let _ = update(&mut app, Msg::ToggleLyrics);
    let _ = update(
        &mut app,
        Msg::LyricsLoaded {
            track_id: "t1".to_owned(),
            result: Ok(LyricsOutcome::Doc(Box::new(LyricsDoc {
                track_id: "t1".to_owned(),
                source: "navidrome".to_owned(),
                match_kind: None,
                synced: false,
                instrumental: false,
                lines: None,
                plain: Some("just words".to_owned()),
            }))),
        },
    );
    assert!(app.lyrics.lines.is_empty());
    assert!(matches!(app.lyrics.doc, Loadable::Ready(_)));
}

#[test]
fn an_unreachable_provider_stays_distinct_from_a_confirmed_absence() {
    // The whole point of the gateway's status codes: "we couldn't check"
    // must not render as "there are none".
    let mut app = playing("t1");
    let _ = update(&mut app, Msg::ToggleLyrics);
    let _ = update(
        &mut app,
        Msg::LyricsLoaded {
            track_id: "t1".to_owned(),
            result: Ok(LyricsOutcome::Unavailable),
        },
    );
    assert!(matches!(
        app.lyrics.doc,
        Loadable::Ready(LyricsOutcome::Unavailable)
    ));
}

#[test]
fn a_transport_failure_lands_as_failed_not_as_an_empty_document() {
    let mut app = playing("t1");
    let _ = update(&mut app, Msg::ToggleLyrics);
    let _ = update(
        &mut app,
        Msg::LyricsLoaded {
            track_id: "t1".to_owned(),
            result: Err("connection refused".to_owned()),
        },
    );
    assert!(matches!(app.lyrics.doc, Loadable::Failed(_)));
}

#[test]
fn following_tracks_the_playhead() {
    let mut app = opened_with_lines("t1");
    app.playback.position = Duration::from_secs(12);
    assert_eq!(super::lyrics::focus_index(&app), Some(1));
}

#[test]
fn the_first_move_starts_from_the_line_being_sung() {
    let mut app = opened_with_lines("t1");
    app.playback.position = Duration::from_secs(12); // line 1 is active
    let _ = update(&mut app, Msg::LyricsMove(1));
    assert!(!app.lyrics.following);
    assert_eq!(app.lyrics.cursor, 2, "reading starts where you were looking");
}

#[test]
fn moving_clamps_at_both_ends() {
    let mut app = opened_with_lines("t1");
    for _ in 0..10 {
        let _ = update(&mut app, Msg::LyricsMove(1));
    }
    assert_eq!(app.lyrics.cursor, 2);
    for _ in 0..10 {
        let _ = update(&mut app, Msg::LyricsMove(-1));
    }
    assert_eq!(app.lyrics.cursor, 0);
}

#[test]
fn moving_in_an_unsynced_document_does_nothing() {
    let mut app = playing("t1");
    let _ = update(&mut app, Msg::ToggleLyrics);
    let _ = update(&mut app, Msg::LyricsMove(1));
    assert!(app.lyrics.following, "there is nothing to read off the playhead");
}

#[test]
fn seeking_to_a_line_resumes_following() {
    let mut app = opened_with_lines("t1");
    let _ = update(&mut app, Msg::LyricsMove(1));
    assert!(!app.lyrics.following);
    let _ = update(&mut app, Msg::LyricsSeek);
    assert!(
        app.lyrics.following,
        "after the seek the playhead is where the reader was looking"
    );
}

#[test]
fn a_new_track_resumes_following_from_the_top() {
    let mut app = opened_with_lines("t1");
    let _ = update(&mut app, Msg::LyricsMove(1));
    app.playback.track_id = Some("t2".to_owned());
    let _ = update(&mut app, Msg::Tick);
    assert!(app.lyrics.following);
    assert_eq!(app.lyrics.cursor, 0);
}

#[test]
fn refresh_forces_a_re_resolve_and_only_one_at_a_time() {
    let mut app = opened_with_lines("t1");
    let effects = update(&mut app, Msg::LyricsRefresh);
    assert_eq!(
        effects,
        vec![Effect::LoadLyrics {
            track_id: "t1".to_owned(),
            force: true
        }]
    );
    assert!(update(&mut app, Msg::LyricsRefresh).is_empty(), "no stacking");

    // The in-flight flag clears on the response, so a second look is possible.
    let _ = update(
        &mut app,
        Msg::LyricsLoaded {
            track_id: "t1".to_owned(),
            result: Ok(synced("t1", vec![line(0, "better")])),
        },
    );
    assert!(!app.lyrics.refreshing);
    assert!(!update(&mut app, Msg::LyricsRefresh).is_empty());
}
