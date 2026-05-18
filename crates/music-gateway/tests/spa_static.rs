//! SPA static-file fallback served by the gateway.
//!
//! When `server.static_dir` is set, the gateway becomes the same-origin host
//! for the React app: `apps/web/dist/` (or whatever is mounted/baked in) is
//! served as a SPA. SPA semantics:
//!
//!   * Hashed asset paths (`/assets/foo.abc.js`) resolve to the file or 404.
//!   * Any other unmatched path falls back to `index.html` so React Router
//!     can pick up the route client-side.
//!   * API routes (`/healthz`, `/v1/*`, `/oauth/*`, `/rest/*`) always win
//!     over static. A static fallback that swallowed `/v1/typo` would
//!     hide API bugs.
//!
//! When `static_dir` is unset, the gateway preserves the legacy
//! `.fallback(not_found)` 404 behaviour — covered by `healthz.rs`.
//!
//! Drives the router via `tower::ServiceExt::oneshot` so no port is bound
//! and tests run in parallel.

use std::fs;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use music_gateway::build_router;
use tempfile::TempDir;

use tower::ServiceExt;

mod common;

/// Build a state whose config has `server.static_dir = Some(tmp.path())` and
/// stash a minimal SPA `dist/` layout in the tmp dir.
async fn state_with_spa(tmp: &TempDir) -> music_gateway::state::AppState {
    fs::write(
        tmp.path().join("index.html"),
        "<!doctype html><title>SPA</title>",
    )
    .expect("write index.html");
    fs::create_dir_all(tmp.path().join("assets")).expect("mkdir assets");
    fs::write(
        tmp.path().join("assets").join("main.abc.js"),
        "console.log('hi');",
    )
    .expect("write main.abc.js");

    let mut cfg = common::test_config();
    cfg.server.static_dir = Some(tmp.path().to_path_buf());
    common::build_state(cfg).await
}

#[tokio::test]
async fn root_serves_index_html_when_static_dir_configured() {
    let tmp = TempDir::new().unwrap();
    let app = build_router(state_with_spa(&tmp).await);

    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(
        String::from_utf8_lossy(&body).contains("<title>SPA</title>"),
        "expected index.html body, got: {:?}",
        String::from_utf8_lossy(&body)
    );
}

#[tokio::test]
async fn asset_path_serves_the_file() {
    let tmp = TempDir::new().unwrap();
    let app = build_router(state_with_spa(&tmp).await);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/assets/main.abc.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"console.log('hi');");
}

#[tokio::test]
async fn unknown_client_route_falls_back_to_index() {
    let tmp = TempDir::new().unwrap();
    let app = build_router(state_with_spa(&tmp).await);

    // `/albums/123` is a React Router route — no file on disk, no
    // gateway API route. SPA semantics: serve index.html so the client
    // bundle can read the URL and render.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/albums/123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("<title>SPA</title>"));
}

#[tokio::test]
async fn healthz_still_wins_over_static() {
    let tmp = TempDir::new().unwrap();
    // Even if a healthz.html exists, the API route must take precedence.
    fs::write(tmp.path().join("healthz"), "WRONG").unwrap();
    let app = build_router(state_with_spa(&tmp).await);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn missing_asset_under_assets_returns_404_not_index() {
    // /assets/<x> is the hashed-bundle prefix. A miss there is a real
    // bug (deploy skew, broken hash) — serving index.html would hide it.
    let tmp = TempDir::new().unwrap();
    let app = build_router(state_with_spa(&tmp).await);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/assets/does-not-exist.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn no_static_dir_keeps_legacy_404_fallback() {
    // Sanity: when static_dir is None, `/` is just an unmatched route.
    let app = build_router(common::build_state(common::test_config()).await);
    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
