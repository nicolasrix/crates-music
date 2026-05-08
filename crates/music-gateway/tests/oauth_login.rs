//! Login form + session cookie. The form is server-rendered HTML (no JS
//! framework needed for a single-user login); the cookie is an opaque
//! 32-byte random token with `Secure; HttpOnly; SameSite=Lax`.

use axum::body::Body;
use axum::http::{Request, StatusCode, header::CONTENT_TYPE};
use http_body_util::BodyExt;
use music_gateway::build_router;
use music_gateway::oauth::{OauthStore, SetupToken, password};
use tower::ServiceExt;

mod common;

const COOKIE_NAME: &str = "gw_session";

async fn bootstrap(oauth: &OauthStore, plaintext: &str) {
    let phc = password::hash(plaintext).unwrap();
    oauth.set_master_password_hash(&phc).await.unwrap();
}

fn extract_set_cookie(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .find_map(|v| {
            let s = v.to_str().ok()?;
            s.starts_with(&format!("{COOKIE_NAME}="))
                .then(|| s.to_string())
        })
}

#[tokio::test]
async fn get_login_renders_form() {
    let oauth = OauthStore::open_in_memory().await.unwrap();
    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;
    let app = build_router(state);

    let resp = app
        .oneshot(Request::get("/oauth/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "text/html; charset=utf-8"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let html = std::str::from_utf8(&body).unwrap();
    assert!(html.contains("<form"), "must render an HTML form: {html}");
    assert!(html.contains("name=\"password\""));
    assert!(html.contains("method=\"post\""));
}

#[tokio::test]
async fn post_login_without_master_password_returns_503() {
    // Gateway not bootstrapped → can't accept logins.
    let oauth = OauthStore::open_in_memory().await.unwrap();
    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::post("/oauth/login")
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("password=anything-very-long"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn post_login_with_wrong_password_returns_401_and_no_cookie() {
    let oauth = OauthStore::open_in_memory().await.unwrap();
    bootstrap(&oauth, "right-password-here").await;
    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::post("/oauth/login")
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("password=wrong-password-here"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(extract_set_cookie(resp.headers()).is_none());
}

#[tokio::test]
async fn post_login_with_correct_password_sets_cookie_and_redirects() {
    let oauth = OauthStore::open_in_memory().await.unwrap();
    bootstrap(&oauth, "right-password-here").await;
    let state =
        common::build_state_with_oauth(common::test_config(), oauth.clone(), SetupToken::none())
            .await;
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::post("/oauth/login")
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("password=right-password-here"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let cookie = extract_set_cookie(resp.headers()).expect("must set session cookie");
    assert!(
        cookie.contains("Secure"),
        "session cookie must have Secure flag: {cookie}"
    );
    assert!(
        cookie.contains("HttpOnly"),
        "session cookie must have HttpOnly flag: {cookie}"
    );
    assert!(
        cookie.to_lowercase().contains("samesite=lax"),
        "session cookie must have SameSite=Lax: {cookie}"
    );
    assert!(
        cookie.contains("Path=/"),
        "session cookie must have Path=/: {cookie}"
    );

    // Default redirect target (no `next` param).
    let location = resp
        .headers()
        .get(axum::http::header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(location, "/");
}

#[tokio::test]
async fn post_login_redirects_to_next_when_relative_path() {
    let oauth = OauthStore::open_in_memory().await.unwrap();
    bootstrap(&oauth, "right-password-here").await;
    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::post("/oauth/login?next=/oauth/authorize?client_id=web")
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("password=right-password-here"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp
        .headers()
        .get(axum::http::header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(location, "/oauth/authorize?client_id=web");
}

#[tokio::test]
async fn post_login_ignores_next_when_absolute_url() {
    // Open-redirect defense: only accept paths starting with a single
    // forward slash (and not protocol-relative `//host`).
    let oauth = OauthStore::open_in_memory().await.unwrap();
    bootstrap(&oauth, "right-password-here").await;
    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;
    let app = build_router(state);

    for evil in [
        "https://evil.example/phish",
        "//evil.example/phish",
        "javascript:alert(1)",
    ] {
        let uri = format!("/oauth/login?next={evil}");
        let resp = app
            .clone()
            .oneshot(
                Request::post(&uri)
                    .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from("password=right-password-here"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        let location = resp
            .headers()
            .get(axum::http::header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(location, "/", "open-redirect must be rejected for: {evil}");
    }
}

#[tokio::test]
async fn session_cookie_actually_authenticates_a_session_lookup() {
    // End-to-end: log in, parse the cookie, and verify the session row
    // exists and is findable via the store. This is what /oauth/authorize
    // will rely on next.
    let oauth = OauthStore::open_in_memory().await.unwrap();
    bootstrap(&oauth, "right-password-here").await;
    let state =
        common::build_state_with_oauth(common::test_config(), oauth.clone(), SetupToken::none())
            .await;
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::post("/oauth/login")
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("password=right-password-here"))
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = extract_set_cookie(resp.headers()).unwrap();
    let token = cookie
        .strip_prefix(&format!("{COOKIE_NAME}="))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let session = oauth.find_session(&token).await.unwrap();
    assert!(
        session.is_some(),
        "session emitted by the login flow must be findable in the store"
    );
}

/// Regression: the login form's `action` URL must percent-encode the `next`
/// value, not just HTML-escape it. Otherwise inner `&` characters bleed
/// into the outer query string when the browser submits the form, the
/// `next` value is truncated at the first `&`, and the post-login redirect
/// drops critical parameters (e.g. `client_id` from the original /oauth/authorize).
#[tokio::test]
async fn get_login_action_url_preserves_full_next_with_multiple_params() {
    let oauth = OauthStore::open_in_memory().await.unwrap();
    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;
    let app = build_router(state);

    // The realistic shape: the inner `next` URL has multiple &-separated
    // params. This is exactly what /oauth/authorize hands us when it
    // redirects to login.
    let inner = "/oauth/authorize?response_type=code&client_id=web&redirect_uri=http%3A%2F%2Flocalhost%3A5173%2Foauth%2Fcallback&code_challenge=abc&code_challenge_method=S256&state=xyz";
    let outer_uri = format!(
        "/oauth/login?next={}",
        // The /oauth/authorize handler does this for us; mirror it here.
        url_percent_encode(inner)
    );

    let resp = app
        .oneshot(Request::get(&outer_uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let html = std::str::from_utf8(&body).unwrap();

    // Parse the form's action attribute the way a browser would. We pull
    // the action="..." literal then re-parse the URL. The crucial
    // assertion is that `next` re-emerges as a single, complete value —
    // not truncated at the first inner `&`.
    let action = extract_action_attr(html).expect("form must have action=\"...\"");
    let parsed =
        url::Url::parse(&format!("http://x{action}")).expect("action must parse as a relative URL");

    let mut next_values: Vec<String> = parsed
        .query_pairs()
        .filter(|(k, _)| k == "next")
        .map(|(_, v)| v.to_string())
        .collect();

    assert_eq!(
        next_values.len(),
        1,
        "expected exactly one `next` param, got {next_values:?}"
    );
    assert_eq!(
        next_values.pop().unwrap(),
        inner,
        "browser must see the full inner URL as the value of `next`, not a truncated version"
    );

    // Sanity: the inner params must NOT appear as their own outer query
    // params. If they do, the encoding leaked.
    for leaked in [
        "client_id",
        "code_challenge",
        "redirect_uri",
        "response_type",
    ] {
        assert!(
            parsed.query_pairs().all(|(k, _)| k != leaked),
            "param {leaked} must not bleed out of `next` into the outer query string"
        );
    }
}

fn extract_action_attr(html: &str) -> Option<String> {
    let needle = "action=\"";
    let start = html.find(needle)? + needle.len();
    let end = html[start..].find('"')? + start;
    Some(html[start..end].to_string())
}

fn url_percent_encode(s: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => write!(&mut out, "%{b:02X}").expect("write to String never fails"),
        }
    }
    out
}
