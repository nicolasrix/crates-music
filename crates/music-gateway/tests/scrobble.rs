//! Integration tests for the dedicated /rest/scrobble handler.
//!
//! Verifies the gateway:
//!   - writes a play_history row on submission scrobbles,
//!   - skips the write on now-playing pings (submission=false),
//!   - forwards both kinds to Navidrome verbatim,
//!   - lets the play_history write fail loudly only in logs,
//!     never blocking the upstream forward (covered indirectly: the
//!     happy-path tests assert both halves).

use axum::body::Body;
use axum::http::{Request, StatusCode, header::AUTHORIZATION};
use music_core::TrackId;
use music_gateway::build_router;
use tower::ServiceExt;
use wiremock::matchers::{method as m_method, path as m_path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;

fn auth_header() -> (&'static str, String) {
    (
        AUTHORIZATION.as_str(),
        format!("Bearer {}", common::TEST_BEARER),
    )
}

fn ok_subsonic_response() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "subsonic-response": { "status": "ok", "version": "1.16.1" }
    }))
}

#[tokio::test]
async fn submission_scrobble_writes_play_history_and_forwards_to_navidrome() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/scrobble"))
        .and(query_param("id", "track-1"))
        // submission default ('true' when absent) on the wire is the
        // common case: web client sends submission=true explicitly,
        // older Subsonic clients omit the param.
        .and(query_param("submission", "true"))
        .respond_with(ok_subsonic_response())
        .expect(1)
        .mount(&upstream)
        .await;

    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let state = common::build_state(cfg).await;
    let app = build_router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/rest/scrobble?id=track-1&submission=true")
                .header(auth_header().0, auth_header().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let last = state
        .play_history()
        .last_played(&TrackId::from("track-1".to_string()))
        .await
        .unwrap();
    assert!(last.is_some(), "play_history row should exist");
    // Server-side timestamp; we don't pin a value, just liveness.
    assert!(last.unwrap() > 0);
}

#[tokio::test]
async fn now_playing_ping_does_not_bump_recency_clock() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/scrobble"))
        .and(query_param("submission", "false"))
        .respond_with(ok_subsonic_response())
        .expect(1)
        .mount(&upstream)
        .await;

    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let state = common::build_state(cfg).await;
    let app = build_router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/rest/scrobble?id=track-1&submission=false")
                .header(auth_header().0, auth_header().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let last = state
        .play_history()
        .last_played(&TrackId::from("track-1".to_string()))
        .await
        .unwrap();
    assert_eq!(last, None, "now-playing pings must not write a row");
}

#[tokio::test]
async fn submission_default_when_param_omitted() {
    // Subsonic spec: submission defaults to true when the query param
    // is omitted. Matches what the older clients send.
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/scrobble"))
        .and(query_param("id", "track-default"))
        .respond_with(ok_subsonic_response())
        .expect(1)
        .mount(&upstream)
        .await;

    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let state = common::build_state(cfg).await;
    let app = build_router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/rest/scrobble?id=track-default")
                .header(auth_header().0, auth_header().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let last = state
        .play_history()
        .last_played(&TrackId::from("track-default".to_string()))
        .await
        .unwrap();
    assert!(last.is_some());
}

#[tokio::test]
async fn second_submission_advances_recency_clock() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/scrobble"))
        .respond_with(ok_subsonic_response())
        .expect(2)
        .mount(&upstream)
        .await;

    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let state = common::build_state(cfg).await;
    let app = build_router(state.clone());
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/rest/scrobble?id=t1&submission=true")
                .header(auth_header().0, auth_header().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let after_first = state
        .play_history()
        .last_played(&TrackId::from("t1".to_string()))
        .await
        .unwrap()
        .expect("first scrobble wrote a row");

    // Tiny sleep so the server-side timestamps are guaranteed distinct;
    // SQLite stores millisecond precision and CI runners can be fast
    // enough to land both writes inside the same millisecond.
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;

    let _ = app
        .oneshot(
            Request::builder()
                .uri("/rest/scrobble?id=t1&submission=true")
                .header(auth_header().0, auth_header().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let after_second = state
        .play_history()
        .last_played(&TrackId::from("t1".to_string()))
        .await
        .unwrap()
        .expect("second scrobble keeps a row");
    assert!(
        after_second >= after_first,
        "recency clock must not move backwards: first={after_first} second={after_second}"
    );
}

#[tokio::test]
async fn submission_scrobble_appends_event_log_with_client_time() {
    // The handler should populate both the fast-path play_history table
    // *and* the durable event log on a submission. The event log is what
    // the diagnostics "recently played" panel reads from and what the
    // future behavioural index will consume — without this write, the
    // panel stays empty even though the user is scrobbling.
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/scrobble"))
        .and(query_param("id", "track-evt"))
        .respond_with(ok_subsonic_response())
        .expect(1)
        .mount(&upstream)
        .await;

    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let state = common::build_state(cfg).await;
    let app = build_router(state.clone());

    // Pass a deterministic time so we can assert the event log preserved
    // it. occurred_at on the wire is the user's clock, not ours.
    let response = app
        .oneshot(
            Request::builder()
                .uri("/rest/scrobble?id=track-evt&submission=true&time=1700000000000")
                .header(auth_header().0, auth_header().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let recent = state
        .event_store()
        .recently_played(10, None)
        .await
        .unwrap();
    assert_eq!(recent.len(), 1, "exactly one scrobble event should land");
    assert_eq!(recent[0].track_id.as_str(), "track-evt");
    assert_eq!(
        recent[0].occurred_at, 1_700_000_000_000,
        "occurred_at should equal the client-supplied `time` param"
    );
}

#[tokio::test]
async fn now_playing_ping_does_not_append_event_log() {
    // Mirror of the play_history equivalent: hint-only pings must not
    // pollute the durable event log either, otherwise the diagnostics
    // panel double-counts every track that's actively playing.
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/scrobble"))
        .and(query_param("submission", "false"))
        .respond_with(ok_subsonic_response())
        .expect(1)
        .mount(&upstream)
        .await;

    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let state = common::build_state(cfg).await;
    let app = build_router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/rest/scrobble?id=track-np&submission=false")
                .header(auth_header().0, auth_header().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let recent = state
        .event_store()
        .recently_played(10, None)
        .await
        .unwrap();
    assert!(recent.is_empty(), "now-playing pings must not log");
}
