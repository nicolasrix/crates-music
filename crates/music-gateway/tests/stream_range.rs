//! Audio-stream byte-range pass-through.
//!
//! The `/rest/stream` proxy must relay HTTP range semantics end-to-end so
//! `<audio>` can seek. Regression guard for the bug where scrubbing forward
//! reset playback to the start: the proxy dropped the client's `Range`
//! request header and Navidrome's `Accept-Ranges`/`Content-Range` response
//! headers, so every seek re-streamed the file from byte 0.
//!
//! Verifies both directions:
//!   - the client's `Range` header reaches upstream (mock `.and(header(..))`);
//!   - upstream's `206` + `Content-Range`/`Accept-Ranges`/`Content-Length`
//!     reach the client verbatim;
//!   - a request with no `Range` is a plain `200` (unchanged behaviour).

use axum::body::Body;
use axum::http::{
    Request, StatusCode,
    header::{ACCEPT_RANGES, AUTHORIZATION, CONTENT_LENGTH, CONTENT_RANGE, RANGE},
};
use music_gateway::build_router;
use tower::ServiceExt;
use wiremock::matchers::{header as m_header, method as m_method, path as m_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;

#[tokio::test]
async fn stream_forwards_range_and_returns_206() {
    let upstream = MockServer::start().await;
    // The mock only matches when the Range header is present — so the
    // `.expect(1)` on drop also asserts the gateway forwarded it upstream.
    Mock::given(m_method("GET"))
        .and(m_path("/rest/stream"))
        .and(m_header("range", "bytes=100-"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-type", "audio/mpeg")
                .insert_header("accept-ranges", "bytes")
                .insert_header("content-range", "bytes 100-999/1000")
                .insert_header("content-length", "900")
                .set_body_bytes(vec![0u8; 900]),
        )
        .expect(1)
        .mount(&upstream)
        .await;

    let app = build_router(
        common::build_state(common::test_config_with_upstream(
            &upstream.uri(),
            "alice",
            "sesame",
        ))
        .await,
    );

    let req = Request::builder()
        .uri("/rest/stream?id=tr-1")
        .header(AUTHORIZATION, format!("Bearer {}", common::TEST_BEARER))
        .header(RANGE, "bytes=100-")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        resp.headers().get(CONTENT_RANGE).unwrap(),
        "bytes 100-999/1000",
        "Content-Range must reach the client so it knows the returned slice"
    );
    assert_eq!(
        resp.headers().get(ACCEPT_RANGES).unwrap(),
        "bytes",
        "Accept-Ranges must reach the client so it treats the stream as seekable"
    );
    assert_eq!(resp.headers().get(CONTENT_LENGTH).unwrap(), "900");
}

#[tokio::test]
async fn stream_without_range_is_plain_200() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/stream"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "audio/mpeg")
                .set_body_bytes(vec![0u8; 1000]),
        )
        .expect(1)
        .mount(&upstream)
        .await;

    let app = build_router(
        common::build_state(common::test_config_with_upstream(
            &upstream.uri(),
            "alice",
            "sesame",
        ))
        .await,
    );

    let req = Request::builder()
        .uri("/rest/stream?id=tr-1")
        .header(AUTHORIZATION, format!("Bearer {}", common::TEST_BEARER))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}
