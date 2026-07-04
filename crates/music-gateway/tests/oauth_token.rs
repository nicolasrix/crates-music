//! POST /oauth/token — RFC 6749 token endpoint with PKCE (RFC 7636)
//! and refresh-token rotation. Two grants today: `authorization_code`
//! and `refresh_token`.

use std::time::Duration;

use axum::body::Body;
use axum::http::header::CONTENT_TYPE;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use music_gateway::build_router;
use music_gateway::oauth::{
    NewAuthCode, NewClient, NewRefreshToken, OauthStore, SetupToken, password, session,
};
use serde_json::Value;
use tower::ServiceExt;

mod common;

// RFC 7636 §4.6 example pair.
const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

async fn store_with_client() -> OauthStore {
    let oauth = OauthStore::open_in_memory().await.unwrap();
    let phc = password::hash("master-password-here").unwrap();
    oauth.set_master_password_hash(&phc).await.unwrap();
    oauth
        .register_client(NewClient {
            client_id: "web".to_string(),
            name: "Web".to_string(),
            redirect_uris: vec!["http://localhost:5173/cb".to_string()],
        })
        .await
        .unwrap();
    oauth
}

async fn issued_code(oauth: &OauthStore) -> String {
    oauth
        .create_auth_code(NewAuthCode {
            user_id: 1,
            client_id: "web".to_string(),
            redirect_uri: "http://localhost:5173/cb".to_string(),
            code_challenge: PKCE_CHALLENGE.to_string(),
            ttl: Duration::from_mins(10),
        })
        .await
        .unwrap()
        .code
}

async fn post_token(app: axum::Router, body: String) -> axum::http::Response<Body> {
    app.oneshot(
        Request::post("/oauth/token")
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn body_json(resp: axum::http::Response<Body>) -> (StatusCode, Value) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, v)
}

// ---------------------------------------------------------------------
// Authorization-code grant
// ---------------------------------------------------------------------

#[tokio::test]
async fn authorization_code_grant_returns_access_and_refresh() {
    let oauth = store_with_client().await;
    let code = issued_code(&oauth).await;
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth.clone(), SetupToken::none())
            .await,
    );

    let body = format!(
        "grant_type=authorization_code\
         &code={code}\
         &client_id=web\
         &redirect_uri=http%3A%2F%2Flocalhost%3A5173%2Fcb\
         &code_verifier={PKCE_VERIFIER}"
    );
    let (status, json) = body_json(post_token(app, body).await).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["access_token"].is_string());
    assert!(json["refresh_token"].is_string());
    assert_eq!(json["token_type"].as_str(), Some("Bearer"));
    assert!(
        json["expires_in"].as_u64().unwrap() > 0,
        "must include positive expires_in"
    );

    // Issued refresh token is real and findable.
    let rt = json["refresh_token"].as_str().unwrap();
    let found = oauth.find_refresh_token(rt).await.unwrap().unwrap();
    assert_eq!(found.client_id, "web");
}

#[tokio::test]
async fn auth_code_is_one_shot_after_grant() {
    let oauth = store_with_client().await;
    let code = issued_code(&oauth).await;
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await,
    );

    let body = format!(
        "grant_type=authorization_code&code={code}&client_id=web\
         &redirect_uri=http%3A%2F%2Flocalhost%3A5173%2Fcb&code_verifier={PKCE_VERIFIER}"
    );
    let first = post_token(app.clone(), body.clone()).await;
    assert_eq!(first.status(), StatusCode::OK);
    let second = post_token(app, body).await;
    assert_eq!(
        second.status(),
        StatusCode::BAD_REQUEST,
        "replaying a consumed auth code must fail"
    );
}

#[tokio::test]
async fn token_rejects_wrong_pkce_verifier() {
    let oauth = store_with_client().await;
    let code = issued_code(&oauth).await;
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await,
    );

    let body = format!(
        "grant_type=authorization_code&code={code}&client_id=web\
         &redirect_uri=http%3A%2F%2Flocalhost%3A5173%2Fcb&code_verifier=wrong-verifier"
    );
    let (status, _) = body_json(post_token(app, body).await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn token_rejects_mismatched_client_id() {
    let oauth = store_with_client().await;
    // Register a second client so the value isn't outright unknown.
    oauth
        .register_client(NewClient {
            client_id: "cli".to_string(),
            name: "CLI".to_string(),
            redirect_uris: vec!["http://x".to_string()],
        })
        .await
        .unwrap();
    let code = issued_code(&oauth).await;
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await,
    );

    let body = format!(
        "grant_type=authorization_code&code={code}&client_id=cli\
         &redirect_uri=http%3A%2F%2Flocalhost%3A5173%2Fcb&code_verifier={PKCE_VERIFIER}"
    );
    let (status, _) = body_json(post_token(app, body).await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn token_rejects_mismatched_redirect_uri() {
    let oauth = store_with_client().await;
    let code = issued_code(&oauth).await;
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await,
    );

    let body = format!(
        "grant_type=authorization_code&code={code}&client_id=web\
         &redirect_uri=http%3A%2F%2Fother%2Fcb&code_verifier={PKCE_VERIFIER}"
    );
    let (status, _) = body_json(post_token(app, body).await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn token_rejects_missing_code_verifier() {
    let oauth = store_with_client().await;
    let code = issued_code(&oauth).await;
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await,
    );

    let body = format!(
        "grant_type=authorization_code&code={code}&client_id=web\
         &redirect_uri=http%3A%2F%2Flocalhost%3A5173%2Fcb"
    );
    let (status, _) = body_json(post_token(app, body).await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn token_rejects_unknown_grant_type() {
    let oauth = store_with_client().await;
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await,
    );
    let (status, _) = body_json(post_token(app, "grant_type=password".to_string()).await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------
// Refresh-token grant
// ---------------------------------------------------------------------

#[tokio::test]
async fn refresh_grant_returns_new_pair_and_revokes_old() {
    let oauth = store_with_client().await;
    let issued = oauth
        .mint_refresh_token(NewRefreshToken {
            user_id: 1,
            client_id: "web".to_string(),
            ttl: None,
            family_id: None,
        })
        .await
        .unwrap();
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth.clone(), SetupToken::none())
            .await,
    );

    let body = format!(
        "grant_type=refresh_token&client_id=web&refresh_token={}",
        issued.token
    );
    let (status, json) = body_json(post_token(app.clone(), body.clone()).await).await;
    assert_eq!(status, StatusCode::OK);
    let new_rt = json["refresh_token"].as_str().unwrap().to_string();
    assert_ne!(new_rt, issued.token, "rotation must mint a different token");

    // Old refresh is now revoked.
    assert!(
        oauth
            .find_refresh_token(&issued.token)
            .await
            .unwrap()
            .is_none(),
        "rotated refresh token must be revoked"
    );

    // New refresh works.
    assert!(oauth.find_refresh_token(&new_rt).await.unwrap().is_some());

    // Replaying the old refresh fails.
    let (replay_status, _) = body_json(post_token(app, body).await).await;
    assert_eq!(replay_status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn refresh_rejects_unknown_token() {
    let oauth = store_with_client().await;
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await,
    );
    let body = "grant_type=refresh_token&client_id=web&refresh_token=ghost".to_string();
    let (status, _) = body_json(post_token(app, body).await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn refresh_rejects_revoked_token() {
    let oauth = store_with_client().await;
    let issued = oauth
        .mint_refresh_token(NewRefreshToken {
            user_id: 1,
            client_id: "web".to_string(),
            ttl: None,
            family_id: None,
        })
        .await
        .unwrap();
    oauth.revoke_refresh_token(&issued.token).await.unwrap();
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await,
    );
    let body = format!(
        "grant_type=refresh_token&client_id=web&refresh_token={}",
        issued.token
    );
    let (status, _) = body_json(post_token(app, body).await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------
// Storage layer
// ---------------------------------------------------------------------

#[tokio::test]
async fn mint_refresh_token_round_trips() {
    let oauth = store_with_client().await;
    let issued = oauth
        .mint_refresh_token(NewRefreshToken {
            user_id: 1,
            client_id: "web".to_string(),
            ttl: Some(Duration::from_hours(1)),
            family_id: None,
        })
        .await
        .unwrap();
    let found = oauth
        .find_refresh_token(&issued.token)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.client_id, "web");
    assert!(found.expires_at_unix_ms.is_some());
}

#[tokio::test]
async fn find_refresh_token_returns_none_for_unknown() {
    let oauth = store_with_client().await;
    assert!(oauth.find_refresh_token("nope").await.unwrap().is_none());
}

#[tokio::test]
async fn consume_refresh_token_is_single_use_under_concurrency() {
    // The core of the atomicity fix: two simultaneous redemptions of the
    // same refresh token must not both succeed (that would mint two valid
    // token pairs from one refresh). Exactly one wins.
    let oauth = store_with_client().await;
    let issued = oauth
        .mint_refresh_token(NewRefreshToken {
            user_id: 1,
            client_id: "web".to_string(),
            ttl: None,
            family_id: None,
        })
        .await
        .unwrap();

    let (a, b) = tokio::join!(
        oauth.consume_refresh_token(&issued.token, "web"),
        oauth.consume_refresh_token(&issued.token, "web"),
    );
    let winners = [a.unwrap(), b.unwrap()].into_iter().flatten().count();
    assert_eq!(winners, 1, "exactly one concurrent consume may win");

    // And the token is now spent for everyone.
    assert!(
        oauth
            .consume_refresh_token(&issued.token, "web")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn consume_refresh_token_wrong_client_does_not_burn_token() {
    // A wrong-client attempt must be rejected *without* revoking the
    // token, so a legitimate client can still use it afterwards.
    let oauth = store_with_client().await;
    let issued = oauth
        .mint_refresh_token(NewRefreshToken {
            user_id: 1,
            client_id: "web".to_string(),
            ttl: None,
            family_id: None,
        })
        .await
        .unwrap();

    assert!(
        oauth
            .consume_refresh_token(&issued.token, "someone-else")
            .await
            .unwrap()
            .is_none(),
        "wrong client must not consume the token"
    );
    // Still valid for the rightful client.
    assert!(
        oauth
            .consume_refresh_token(&issued.token, "web")
            .await
            .unwrap()
            .is_some(),
        "rightful client can still redeem"
    );
}

// ---------------------------------------------------------------------
// Refresh-token reuse detection (sec review 1.5)
// ---------------------------------------------------------------------

#[tokio::test]
async fn rotation_preserves_the_refresh_token_family() {
    // A rotated pair must stay in the same family so reuse of any earlier
    // member can revoke the whole chain.
    let oauth = store_with_client().await;
    let t1 = oauth
        .mint_refresh_token(NewRefreshToken {
            user_id: 1,
            client_id: "web".to_string(),
            ttl: None,
            family_id: None,
        })
        .await
        .unwrap();
    let c1 = oauth
        .consume_refresh_token(&t1.token, "web")
        .await
        .unwrap()
        .unwrap();
    // A brand-new grant is its own family (id = its own hash).
    assert_eq!(c1.family_id, t1.token_hash);

    // The successor inherits that family.
    let t2 = oauth
        .mint_refresh_token(NewRefreshToken {
            user_id: 1,
            client_id: "web".to_string(),
            ttl: None,
            family_id: Some(c1.family_id.clone()),
        })
        .await
        .unwrap();
    let c2 = oauth
        .consume_refresh_token(&t2.token, "web")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(c2.family_id, c1.family_id, "rotation keeps one family");
}

#[tokio::test]
async fn detect_refresh_reuse_flags_replayed_rotated_token_outside_grace() {
    // A token that was rotated (revoked) and is replayed after the grace
    // window is the fingerprint of theft — detection returns its family.
    let oauth = store_with_client().await;
    let t1 = oauth
        .mint_refresh_token(NewRefreshToken {
            user_id: 1,
            client_id: "web".to_string(),
            ttl: None,
            family_id: None,
        })
        .await
        .unwrap();
    oauth.consume_refresh_token(&t1.token, "web").await.unwrap();

    // Zero grace → a just-revoked token already counts as reuse.
    let family = oauth
        .detect_refresh_reuse(&t1.token, "web", Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(family.as_deref(), Some(t1.token_hash.as_str()));

    // An unknown token is never flagged.
    assert!(
        oauth
            .detect_refresh_reuse("ghost", "web", Duration::ZERO)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn detect_refresh_reuse_ignores_just_revoked_token_within_grace() {
    // A large grace window suppresses the benign concurrent double-submit:
    // a token revoked "just now" is not treated as theft.
    let oauth = store_with_client().await;
    let t1 = oauth
        .mint_refresh_token(NewRefreshToken {
            user_id: 1,
            client_id: "web".to_string(),
            ttl: None,
            family_id: None,
        })
        .await
        .unwrap();
    oauth.consume_refresh_token(&t1.token, "web").await.unwrap();

    let family = oauth
        .detect_refresh_reuse(&t1.token, "web", Duration::from_hours(1))
        .await
        .unwrap();
    assert!(family.is_none(), "within grace must not flag reuse");
}

#[tokio::test]
async fn revoke_family_kills_the_chain_and_derived_access_tokens() {
    let oauth = store_with_client().await;
    let t1 = oauth
        .mint_refresh_token(NewRefreshToken {
            user_id: 1,
            client_id: "web".to_string(),
            ttl: None,
            family_id: None,
        })
        .await
        .unwrap();
    // A live successor in the same family, with a derived access token.
    let t2 = oauth
        .mint_refresh_token(NewRefreshToken {
            user_id: 1,
            client_id: "web".to_string(),
            ttl: None,
            family_id: Some(t1.token_hash.clone()),
        })
        .await
        .unwrap();
    let a2 = oauth
        .mint_access_token("web", Some(&t2.token_hash), Duration::from_hours(1))
        .await
        .unwrap();

    let revoked = oauth.revoke_family(&t1.token_hash).await.unwrap();
    assert!(revoked >= 1, "family revocation touches live members");
    assert!(
        oauth.find_refresh_token(&t2.token).await.unwrap().is_none(),
        "successor refresh token is revoked"
    );
    assert!(
        oauth.find_access_token(&a2.token).await.unwrap().is_none(),
        "access token derived from the family is revoked"
    );
}

#[tokio::test]
async fn refresh_reuse_over_http_revokes_the_whole_family() {
    // End-to-end: rotate once, then replay the original (aged past the
    // grace window) — the endpoint must revoke the successor pair too.
    let oauth = store_with_client().await;
    let t1 = oauth
        .mint_refresh_token(NewRefreshToken {
            user_id: 1,
            client_id: "web".to_string(),
            ttl: None,
            family_id: None,
        })
        .await
        .unwrap();
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth.clone(), SetupToken::none())
            .await,
    );

    // First refresh rotates T1 → T2 (+ access A2), same family.
    let body = format!("grant_type=refresh_token&client_id=web&refresh_token={}", t1.token);
    let (status, json) = body_json(post_token(app.clone(), body.clone()).await).await;
    assert_eq!(status, StatusCode::OK);
    let t2 = json["refresh_token"].as_str().unwrap().to_string();
    let a2 = json["access_token"].as_str().unwrap().to_string();
    assert!(oauth.find_refresh_token(&t2).await.unwrap().is_some());

    // Age T1's revocation past the grace window so the replay reads as
    // theft rather than a concurrent double-submit.
    sqlx::query("UPDATE refresh_tokens SET revoked_at = 1 WHERE token_hash = ?")
        .bind(session::hash_token(&t1.token))
        .execute(oauth.pool())
        .await
        .unwrap();

    // Replay T1 → invalid_grant AND family revocation.
    let (replay_status, _) = body_json(post_token(app, body).await).await;
    assert_eq!(replay_status, StatusCode::BAD_REQUEST);
    assert!(
        oauth.find_refresh_token(&t2).await.unwrap().is_none(),
        "reuse must revoke the successor refresh token"
    );
    assert!(
        oauth.find_access_token(&a2).await.unwrap().is_none(),
        "reuse must revoke the successor access token"
    );
}
