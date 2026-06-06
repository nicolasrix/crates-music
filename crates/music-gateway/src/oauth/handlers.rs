//! HTTP handlers for the OAuth surface.
//!
//! P3.1.b shipped the bootstrap endpoint; P3.1.c adds login + sessions.
//! Authorize, token, and revoke arrive in subsequent sub-phases.

use std::time::Duration;

use std::net::{IpAddr, SocketAddr};

use axum::Form;
use axum::Json;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::header::{COOKIE, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use url::Url;

use crate::oauth::password;
use crate::oauth::storage::{DeviceCodeRow, DevicePollState, NewAuthCode, NewDeviceCode, NewRefreshToken};
use crate::state::AppState;

/// Cookie name. Short, gateway-scoped, matches the convention of OAuth
/// sample servers.
pub const SESSION_COOKIE: &str = "gw_session";

/// Default browser session lifetime. Long enough that re-login is rare,
/// short enough that a stolen laptop's session expires.
pub const SESSION_TTL: Duration = Duration::from_hours(24);

/// Authorization codes are short-lived per OAuth 2.1 (RFC 6749 §10.5
/// recommends ≤ 10 min).
pub const AUTH_CODE_TTL: Duration = Duration::from_mins(10);

/// Access tokens are short-lived; clients refresh as needed.
pub const ACCESS_TOKEN_TTL: Duration = Duration::from_hours(1);

/// Device codes are short-lived (RFC 8628 §3.2 example uses ~15 min; we
/// match the auth-code window of 10 min).
pub const DEVICE_CODE_TTL: Duration = Duration::from_mins(10);

/// Default device-flow polling interval handed to the client (RFC 8628
/// §3.2 default is 5 s).
pub const DEVICE_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Minimum master-password length. NIST SP 800-63B recommends ≥ 8 with
/// no other rules; we go a little stricter (this is the *root* credential).
pub const MIN_PASSWORD_LEN: usize = 12;

#[derive(Debug, Deserialize)]
pub struct SetupForm {
    pub token: String,
    pub password: String,
}

/// POST /oauth/setup
///
/// One-shot bootstrap. Accepts the setup token printed at startup plus a
/// master password; hashes the password (Argon2id) and stores it. Once a
/// master password exists, the endpoint returns 410 Gone forever.
pub async fn setup(
    State(state): State<AppState>,
    Form(form): Form<SetupForm>,
) -> Result<StatusCode, (StatusCode, String)> {
    // Already configured → endpoint is permanently disabled, regardless
    // of whether the in-memory token is still set.
    if state
        .oauth()
        .master_password_hash()
        .await
        .map_err(internal)?
        .is_some()
    {
        return Err((
            StatusCode::GONE,
            "gateway is already configured".to_string(),
        ));
    }

    // No active token → never was one, or it's already been consumed.
    if !state.setup_token().is_active() {
        return Err((StatusCode::GONE, "setup token is not active".to_string()));
    }

    if !state.setup_token().matches(&form.token) {
        return Err((StatusCode::FORBIDDEN, "invalid setup token".to_string()));
    }

    if form.password.len() < MIN_PASSWORD_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("password must be at least {MIN_PASSWORD_LEN} characters"),
        ));
    }

    let phc = password::hash(&form.password).map_err(|e| {
        tracing::error!("argon2 hashing failed: {e}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not hash password".to_string(),
        )
    })?;
    state
        .oauth()
        .set_master_password_hash(&phc)
        .await
        .map_err(internal)?;

    // Burn the token only on success — a failed attempt shouldn't lock
    // the operator out.
    let _ = state.setup_token().consume();

    Ok(StatusCode::OK)
}

fn internal<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    tracing::error!("oauth setup: {e}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal error".to_string(),
    )
}

// ---------------------------------------------------------------------
// Login
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
pub struct LoginQuery {
    /// Optional post-login redirect target. Only honoured when it's a
    /// safe relative path — see `safe_redirect_target`.
    pub next: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub password: String,
}

/// GET /oauth/login — server-rendered HTML form.
pub async fn login_get(Query(q): Query<LoginQuery>) -> Html<String> {
    Html(render_login(None, q.next.as_deref()))
}

/// POST /oauth/login — verifies the password, mints a session, sets the
/// cookie, and redirects (303 See Other).
pub async fn login_post(
    State(state): State<AppState>,
    connect: Option<ConnectInfo<SocketAddr>>,
    Query(q): Query<LoginQuery>,
    Form(form): Form<LoginForm>,
) -> Result<Response, (StatusCode, String)> {
    // Peer address as the limiter key. Falls back to an unspecified
    // address when connection info is absent (e.g. unit tests via
    // `oneshot`), which buckets such callers together — fine, they share
    // one throttle. See `LoginLimiter` for the reverse-proxy caveat.
    let ip = connect.map_or(IpAddr::from([0, 0, 0, 0]), |ci| ci.0.ip());
    if let Err(remaining) = state.login_limiter().check(ip) {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            format!(
                "too many login attempts; retry in {}s",
                remaining.as_secs() + 1
            ),
        ));
    }

    // Uniform failure: an unauthenticated caller must not be able to
    // tell "gateway not bootstrapped" from "wrong password" — both
    // return an identical 401. When no master password is stored we
    // still burn an Argon2id verify (`verify_absent`) so the two paths
    // are timing-indistinguishable as well as response-identical. The
    // one-time setup URL printed at startup is how the operator
    // bootstraps; the login form never needs to disclose that state.
    let phc = state
        .oauth()
        .master_password_hash()
        .await
        .map_err(internal)?;
    let ok = match phc {
        Some(phc) => password::verify(&form.password, &phc).map_err(internal)?,
        None => password::verify_absent(&form.password),
    };
    if !ok {
        state.login_limiter().record_failure(ip);
        return Err((StatusCode::UNAUTHORIZED, "invalid credentials".to_string()));
    }
    state.login_limiter().record_success(ip);

    let issued = state
        .oauth()
        .create_session(SESSION_TTL)
        .await
        .map_err(internal)?;

    let target = safe_redirect_target(q.next.as_deref());
    let cookie = format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={ttl}",
        token = issued.token,
        ttl = SESSION_TTL.as_secs(),
    );

    let mut resp = (StatusCode::SEE_OTHER, "").into_response();
    resp.headers_mut().insert(
        SET_COOKIE,
        cookie.parse().expect("Set-Cookie value uses only ASCII"),
    );
    resp.headers_mut().insert(
        LOCATION,
        target.parse().expect("redirect target is ASCII-safe"),
    );
    Ok(resp)
}

/// Whitelist `next` values to single-leading-slash relative paths. Drops
/// `//host/...` (protocol-relative), `https://...` (cross-origin),
/// `javascript:` (XSS via redirect). Defaults to `/` on rejection.
fn safe_redirect_target(next: Option<&str>) -> String {
    match next {
        Some(n)
            if n.starts_with('/')
                && !n.starts_with("//")
                && !n.contains([' ', '\r', '\n', '\t']) =>
        {
            n.to_string()
        }
        _ => "/".to_string(),
    }
}

fn render_login(error: Option<&str>, next: Option<&str>) -> String {
    let action_query = match next {
        Some(n) => format!("?next={}", urlencoding_encode(n)),
        None => String::new(),
    };
    let error_block = match error {
        Some(msg) => format!(r#"<p class="error">{}</p>"#, html_escape(msg)),
        None => String::new(),
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>music gateway · sign in</title>
<style>
body {{ font-family: system-ui, sans-serif; max-width: 22rem; margin: 4rem auto; padding: 0 1rem; }}
form {{ display: flex; flex-direction: column; gap: 0.75rem; }}
input, button {{ padding: 0.5rem; font-size: 1rem; }}
.error {{ color: #b00; padding: 0.5rem 0; }}
</style>
</head>
<body>
<h1>music gateway</h1>
{error_block}
<form method="post" action="/oauth/login{action_query}">
<label>master password<input type="password" name="password" required autofocus></label>
<button type="submit">sign in</button>
</form>
</body>
</html>"#
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

// ---------------------------------------------------------------------
// Authorize (OAuth 2.1 Authorization Code + PKCE)
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AuthorizeQuery {
    pub response_type: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: Option<String>,
    pub code_challenge_method: Option<String>,
    pub state: Option<String>,
    /// Scopes are unused (single-user gateway has nothing to gate); we
    /// still accept the param to be a polite OAuth citizen.
    #[serde(default)]
    pub scope: Option<String>,
}

/// GET /oauth/authorize
pub async fn authorize(
    State(state): State<AppState>,
    Query(q): Query<AuthorizeQuery>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, String)> {
    // 1. Strict validation of authorization-request shape *before* even
    //    looking at the session — these errors mean the request is
    //    malformed regardless of who's signed in.
    if q.response_type != "code" {
        return Err((
            StatusCode::BAD_REQUEST,
            "only response_type=code is supported".to_string(),
        ));
    }
    let code_challenge = q.code_challenge.as_deref().ok_or((
        StatusCode::BAD_REQUEST,
        "PKCE is mandatory: code_challenge is required".to_string(),
    ))?;
    if q.code_challenge_method.as_deref() != Some("S256") {
        return Err((
            StatusCode::BAD_REQUEST,
            "only code_challenge_method=S256 is supported".to_string(),
        ));
    }

    // 2. Validate client + redirect_uri. The redirect_uri MUST exactly
    //    match a registered URI — that's the spec, and it's the
    //    open-redirect defense.
    let client = state
        .oauth()
        .find_client(&q.client_id)
        .await
        .map_err(internal)?
        .ok_or((StatusCode::BAD_REQUEST, "unknown client_id".to_string()))?;
    if !client.redirect_uris.iter().any(|u| u == &q.redirect_uri) {
        return Err((
            StatusCode::BAD_REQUEST,
            "redirect_uri is not registered for this client".to_string(),
        ));
    }

    // 3. Session check. If absent or invalid, redirect to login with the
    //    full authorize URL as `next` so the user is bounced back here
    //    after signing in.
    let session_token = extract_session_cookie(&headers);
    let signed_in = match session_token.as_deref() {
        Some(t) => state
            .oauth()
            .find_session(t)
            .await
            .map_err(internal)?
            .is_some(),
        None => false,
    };
    if !signed_in {
        let mut next = String::from("/oauth/authorize?");
        let mut params: Vec<(&str, &str)> = vec![
            ("response_type", q.response_type.as_str()),
            ("client_id", q.client_id.as_str()),
            ("redirect_uri", q.redirect_uri.as_str()),
            ("code_challenge", code_challenge),
            ("code_challenge_method", "S256"),
        ];
        if let Some(s) = q.state.as_deref() {
            params.push(("state", s));
        }
        for (i, (k, v)) in params.iter().enumerate() {
            if i > 0 {
                next.push('&');
            }
            next.push_str(k);
            next.push('=');
            next.push_str(&urlencoding_encode(v));
        }
        let login = format!("/oauth/login?next={}", urlencoding_encode(&next));
        return Ok(redirect(&login));
    }

    // 4. Mint the code, store it, redirect back to client.
    let issued = state
        .oauth()
        .create_auth_code(NewAuthCode {
            client_id: q.client_id.clone(),
            redirect_uri: q.redirect_uri.clone(),
            code_challenge: code_challenge.to_string(),
            ttl: AUTH_CODE_TTL,
        })
        .await
        .map_err(internal)?;

    let mut url = Url::parse(&q.redirect_uri).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("redirect_uri is not a valid URL: {e}"),
        )
    })?;
    url.query_pairs_mut().append_pair("code", &issued.code);
    if let Some(s) = &q.state {
        url.query_pairs_mut().append_pair("state", s);
    }
    Ok(redirect(url.as_str()))
}

/// Build a 303 See Other response with `Location` set. No body.
fn redirect(target: &str) -> Response {
    let mut resp = (StatusCode::SEE_OTHER, "").into_response();
    resp.headers_mut().insert(
        LOCATION,
        target
            .parse()
            .expect("redirect target is constructed from URL-safe input"),
    );
    resp
}

/// Find `gw_session=<value>` in the `Cookie` header. Returns `None` if
/// the header is missing or doesn't contain the cookie.
fn extract_session_cookie(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(COOKIE)?.to_str().ok()?;
    for pair in raw.split(';') {
        let pair = pair.trim();
        if let Some(rest) = pair.strip_prefix(&format!("{SESSION_COOKIE}=")) {
            return Some(rest.to_string());
        }
    }
    None
}

/// Minimal URL component encoder — escapes the characters that can
/// terminate or change the meaning of a query string. Avoids dragging
/// `percent-encoding` in just for this one call site.
fn urlencoding_encode(s: &str) -> String {
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

// ---------------------------------------------------------------------
// Token (RFC 6749 §3.2 + RFC 7636 §4.6)
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TokenForm {
    pub grant_type: String,
    pub code: Option<String>,
    pub client_id: Option<String>,
    pub redirect_uri: Option<String>,
    pub code_verifier: Option<String>,
    pub refresh_token: Option<String>,
    /// RFC 8628 device-code grant.
    pub device_code: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: &'static str,
    pub expires_in: u64,
}

#[derive(Debug, Serialize)]
pub struct OauthError {
    pub error: &'static str,
    pub error_description: String,
}

/// POST /oauth/token
pub async fn token(
    State(state): State<AppState>,
    Form(form): Form<TokenForm>,
) -> Result<Json<TokenResponse>, (StatusCode, Json<OauthError>)> {
    match form.grant_type.as_str() {
        "authorization_code" => grant_authorization_code(&state, form).await,
        "refresh_token" => grant_refresh_token(&state, form).await,
        "urn:ietf:params:oauth:grant-type:device_code" => grant_device_code(&state, form).await,
        other => Err(oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            format!("unsupported grant_type: {other}"),
        )),
    }
}

async fn grant_authorization_code(
    state: &AppState,
    form: TokenForm,
) -> Result<Json<TokenResponse>, (StatusCode, Json<OauthError>)> {
    let code = form
        .code
        .ok_or_else(|| oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "missing code"))?;
    let client_id = form.client_id.ok_or_else(|| {
        oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "missing client_id",
        )
    })?;
    let redirect_uri = form.redirect_uri.ok_or_else(|| {
        oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "missing redirect_uri",
        )
    })?;
    let code_verifier = form.code_verifier.ok_or_else(|| {
        oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "missing code_verifier (PKCE is mandatory)",
        )
    })?;

    let consumed = state
        .oauth()
        .consume_auth_code(&code)
        .await
        .map_err(|e| oauth_internal(&e))?
        .ok_or_else(|| {
            oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "authorization code is invalid, expired, or already used",
            )
        })?;

    if consumed.client_id != client_id {
        return Err(oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "client_id does not match the authorization code",
        ));
    }
    if consumed.redirect_uri != redirect_uri {
        return Err(oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "redirect_uri does not match the authorization code",
        ));
    }

    let derived = pkce_s256(&code_verifier);
    if !ct_eq_str(&derived, &consumed.code_challenge) {
        return Err(oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "PKCE verification failed",
        ));
    }

    let pair = mint_pair(state, &client_id).await?;
    Ok(Json(pair))
}

async fn grant_refresh_token(
    state: &AppState,
    form: TokenForm,
) -> Result<Json<TokenResponse>, (StatusCode, Json<OauthError>)> {
    let refresh = form.refresh_token.ok_or_else(|| {
        oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "missing refresh_token",
        )
    })?;
    let client_id = form.client_id.ok_or_else(|| {
        oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "missing client_id",
        )
    })?;

    // Rotation is a single atomic step: `consume_refresh_token` revokes
    // the presented token and returns its row exactly once, scoped to
    // this client. A concurrent replay of the same token loses the race
    // and gets `None` here — closing the find-then-revoke window that
    // would otherwise let one refresh mint two valid pairs. A wrong
    // client_id, an unknown/expired/revoked token, and a replay all
    // collapse to the same `invalid_grant` (no oracle, and a
    // wrong-client attempt does not burn a valid token).
    let _consumed = state
        .oauth()
        .consume_refresh_token(&refresh, &client_id)
        .await
        .map_err(|e| oauth_internal(&e))?
        .ok_or_else(|| {
            oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "refresh token is invalid, expired, revoked, or for a different client",
            )
        })?;

    let pair = mint_pair(state, &client_id).await?;
    Ok(Json(pair))
}

/// RFC 8628 §3.4 device-code grant: the polling step. Maps the device
/// code's state to the spec's polling errors, or mints a token pair once
/// the user has approved.
async fn grant_device_code(
    state: &AppState,
    form: TokenForm,
) -> Result<Json<TokenResponse>, (StatusCode, Json<OauthError>)> {
    let device_code = form.device_code.ok_or_else(|| {
        oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "missing device_code",
        )
    })?;
    let client_id = form.client_id.ok_or_else(|| {
        oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "missing client_id",
        )
    })?;

    match state
        .oauth()
        .consume_device_code(&device_code)
        .await
        .map_err(|e| oauth_internal(&e))?
    {
        DevicePollState::Pending => Err(oauth_error(
            StatusCode::BAD_REQUEST,
            "authorization_pending",
            "the user has not yet approved this device",
        )),
        DevicePollState::Denied => Err(oauth_error(
            StatusCode::BAD_REQUEST,
            "access_denied",
            "the authorization request was denied",
        )),
        DevicePollState::Expired => Err(oauth_error(
            StatusCode::BAD_REQUEST,
            "expired_token",
            "the device code has expired or is no longer valid",
        )),
        // The device_code is the high-entropy secret; client_id is not.
        // We still cross-check it for correctness — a mismatch can only
        // come from a caller already holding the secret code (the CLI).
        DevicePollState::Approved {
            client_id: code_client,
        } => {
            if code_client != client_id {
                return Err(oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "client_id does not match the device code",
                ));
            }
            let pair = mint_pair(state, &client_id).await?;
            Ok(Json(pair))
        }
    }
}

async fn mint_pair(
    state: &AppState,
    client_id: &str,
) -> Result<TokenResponse, (StatusCode, Json<OauthError>)> {
    let refresh = state
        .oauth()
        .mint_refresh_token(NewRefreshToken {
            client_id: client_id.to_string(),
            ttl: None, // rotation handles revocation
        })
        .await
        .map_err(|e| oauth_internal(&e))?;
    let access = state
        .oauth()
        .mint_access_token(client_id, Some(&refresh.token_hash), ACCESS_TOKEN_TTL)
        .await
        .map_err(|e| oauth_internal(&e))?;
    Ok(TokenResponse {
        access_token: access.token,
        refresh_token: refresh.token,
        token_type: "Bearer",
        expires_in: ACCESS_TOKEN_TTL.as_secs(),
    })
}

/// PKCE S256: `base64url(sha256(verifier))` with no padding.
fn pkce_s256(verifier: &str) -> String {
    let mut h = Sha256::new();
    h.update(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(h.finalize())
}

fn ct_eq_str(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.as_bytes().ct_eq(b.as_bytes()).into()
}

fn oauth_error(
    status: StatusCode,
    code: &'static str,
    description: impl Into<String>,
) -> (StatusCode, Json<OauthError>) {
    (
        status,
        Json(OauthError {
            error: code,
            error_description: description.into(),
        }),
    )
}

fn oauth_internal<E: std::fmt::Display>(e: &E) -> (StatusCode, Json<OauthError>) {
    tracing::error!("oauth token: {e}");
    oauth_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "server_error",
        "internal error",
    )
}

// ---------------------------------------------------------------------
// Revoke (RFC 7009)
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RevokeForm {
    pub token: String,
    /// Optional hint per RFC 7009. We don't trust it — always probe both
    /// tables — but we accept the field for spec compliance.
    #[serde(default)]
    #[allow(dead_code)]
    pub token_type_hint: Option<String>,
}

/// POST /oauth/revoke
///
/// RFC 7009: the server MUST respond 200 even if the token is unknown,
/// to avoid leaking which tokens exist. We probe refresh first
/// (revoking a refresh cascades to its access tokens), then access.
pub async fn revoke(State(state): State<AppState>, Form(form): Form<RevokeForm>) -> StatusCode {
    let oauth = state.oauth();
    if let Err(e) = oauth.revoke_refresh_token(&form.token).await {
        tracing::error!("revoke refresh: {e}");
    }
    if let Err(e) = oauth.revoke_access_token(&form.token).await {
        tracing::error!("revoke access: {e}");
    }
    StatusCode::OK
}

// ---------------------------------------------------------------------
// Device Authorization Grant (RFC 8628)
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct DeviceAuthForm {
    pub client_id: String,
    /// Scopes are unused (single-user gateway); accepted for spec politeness.
    #[serde(default)]
    #[allow(dead_code)]
    pub scope: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DeviceAuthResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// POST /oauth/device_authorization (RFC 8628 §3.1/§3.2). Public client
/// asks for a device code; we mint a device/user code pair and tell the
/// client where to send the user and how often to poll.
pub async fn device_authorization(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<DeviceAuthForm>,
) -> Result<Json<DeviceAuthResponse>, (StatusCode, Json<OauthError>)> {
    // The client must be registered (mirrors authorize()'s client check).
    state
        .oauth()
        .find_client(&form.client_id)
        .await
        .map_err(|e| oauth_internal(&e))?
        .ok_or_else(|| {
            oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_client",
                "unknown client_id",
            )
        })?;

    let issued = state
        .oauth()
        .create_device_code(NewDeviceCode {
            client_id: form.client_id,
            ttl: DEVICE_CODE_TTL,
            interval: DEVICE_POLL_INTERVAL,
        })
        .await
        .map_err(|e| oauth_internal(&e))?;

    let base = request_base_url(&headers);
    let verification_uri = format!("{base}/oauth/device");
    let verification_uri_complete = format!(
        "{verification_uri}?user_code={}",
        urlencoding_encode(&issued.user_code)
    );
    Ok(Json(DeviceAuthResponse {
        device_code: issued.device_code,
        user_code: issued.user_code,
        verification_uri,
        verification_uri_complete,
        expires_in: DEVICE_CODE_TTL.as_secs(),
        interval: u64::try_from(issued.interval_secs).unwrap_or(5),
    }))
}

#[derive(Debug, Deserialize, Default)]
pub struct DeviceVerifyQuery {
    pub user_code: Option<String>,
}

/// GET /oauth/device — the verification page. Session-gated: an
/// unauthenticated visitor is bounced to login and back. With a
/// `user_code` it shows the code + Approve/Deny; without one it shows a
/// field to enter the code.
pub async fn device_verify_get(
    State(state): State<AppState>,
    Query(q): Query<DeviceVerifyQuery>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, String)> {
    if !is_signed_in(&state, &headers).await? {
        return Ok(redirect(&device_login_redirect(q.user_code.as_deref())));
    }

    let page = match q.user_code.as_deref() {
        None => render_device_entry(),
        Some(user_code) => {
            match state
                .oauth()
                .find_device_by_user_code(user_code)
                .await
                .map_err(internal)?
            {
                None => render_device_message(
                    "Unknown code",
                    "That code wasn't found. Check the code shown in your terminal.",
                ),
                Some(row) => render_device_for_row(&row),
            }
        }
    };
    Ok(Html(page).into_response())
}

#[derive(Debug, Deserialize)]
pub struct DeviceVerifyForm {
    pub user_code: String,
    /// `approve` or `deny`.
    pub action: String,
}

/// POST /oauth/device — record the approve/deny decision. Session-gated.
pub async fn device_verify_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<DeviceVerifyForm>,
) -> Result<Response, (StatusCode, String)> {
    if !is_signed_in(&state, &headers).await? {
        return Ok(redirect(&device_login_redirect(Some(&form.user_code))));
    }

    let approve = match form.action.as_str() {
        "approve" => true,
        "deny" => false,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "action must be 'approve' or 'deny'".to_string(),
            ));
        }
    };

    let recorded = state
        .oauth()
        .set_device_decision(&form.user_code, approve)
        .await
        .map_err(internal)?;

    let page = if !recorded {
        render_device_message(
            "Code not available",
            "That code is unknown, already decided, or expired. Start over from your terminal.",
        )
    } else if approve {
        render_device_message(
            "Device approved",
            "You can return to your terminal — it will finish signing in shortly.",
        )
    } else {
        render_device_message(
            "Device denied",
            "The sign-in request was denied. You can close this tab.",
        )
    };
    Ok(Html(page).into_response())
}

/// Is there a valid browser session on this request? DB errors surface as
/// a 500 (mirrors `authorize`'s `map_err(internal)`).
async fn is_signed_in(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<bool, (StatusCode, String)> {
    let Some(token) = extract_session_cookie(headers) else {
        return Ok(false);
    };
    Ok(state
        .oauth()
        .find_session(&token)
        .await
        .map_err(internal)?
        .is_some())
}

/// Build the `/oauth/login?next=…` URL that bounces an unauthenticated
/// visitor back to the device page (preserving the `user_code`).
fn device_login_redirect(user_code: Option<&str>) -> String {
    let next = match user_code {
        Some(uc) => format!("/oauth/device?user_code={}", urlencoding_encode(uc)),
        None => "/oauth/device".to_string(),
    };
    format!("/oauth/login?next={}", urlencoding_encode(&next))
}

/// Classify a device-code row for the verification page: show the
/// Approve/Deny form only while it's genuinely pending.
fn render_device_for_row(row: &DeviceCodeRow) -> String {
    let now = now_unix_ms();
    if row.consumed_at_unix_ms.is_some() || row.expires_at_unix_ms <= now {
        render_device_message(
            "Code expired",
            "This code has expired or was already used. Start over from your terminal.",
        )
    } else if row.approved_at_unix_ms.is_some() {
        render_device_message(
            "Already approved",
            "This device was already approved. Return to your terminal.",
        )
    } else if row.denied_at_unix_ms.is_some() {
        render_device_message("Already denied", "This sign-in request was denied.")
    } else {
        render_device_confirm(&row.user_code, &row.client_id)
    }
}

/// The Approve/Deny confirmation page for a pending code.
fn render_device_confirm(user_code: &str, client_id: &str) -> String {
    let uc = html_escape(user_code);
    let cid = html_escape(client_id);
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>music gateway · authorize device</title>
<style>
{DEVICE_PAGE_CSS}
.code {{ font-family: ui-monospace, monospace; font-size: 1.6rem; letter-spacing: 0.15em; padding: 0.75rem; background: #f2f2f2; border-radius: 0.4rem; text-align: center; }}
.row {{ display: flex; gap: 0.75rem; margin-top: 1rem; }}
button {{ padding: 0.6rem 1rem; font-size: 1rem; cursor: pointer; }}
.approve {{ background: #1a7f37; color: #fff; border: none; border-radius: 0.4rem; }}
.deny {{ background: #fff; color: #b00; border: 1px solid #b00; border-radius: 0.4rem; }}
</style>
</head>
<body>
<h1>Authorize device</h1>
<p>A device (<strong>{cid}</strong>) is requesting access. Confirm this code matches the one shown in your terminal:</p>
<div class="code">{uc}</div>
<div class="row">
<form method="post" action="/oauth/device">
<input type="hidden" name="user_code" value="{uc}">
<input type="hidden" name="action" value="approve">
<button class="approve" type="submit">Approve this device</button>
</form>
<form method="post" action="/oauth/device">
<input type="hidden" name="user_code" value="{uc}">
<input type="hidden" name="action" value="deny">
<button class="deny" type="submit">Deny</button>
</form>
</div>
</body>
</html>"#
    )
}

/// The "enter your code" page shown when no `user_code` is in the URL.
fn render_device_entry() -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>music gateway · authorize device</title>
<style>
{DEVICE_PAGE_CSS}
form {{ display: flex; flex-direction: column; gap: 0.75rem; }}
input[type=text] {{ padding: 0.5rem; font-size: 1.2rem; font-family: ui-monospace, monospace; letter-spacing: 0.1em; text-transform: uppercase; }}
button {{ padding: 0.6rem; font-size: 1rem; cursor: pointer; }}
</style>
</head>
<body>
<h1>Authorize device</h1>
<p>Enter the code shown in your terminal:</p>
<form method="get" action="/oauth/device">
<input type="text" name="user_code" placeholder="XXXX-XXXX" autofocus required>
<button type="submit">Continue</button>
</form>
</body>
</html>"#
    )
}

/// A terminal status page (approved / denied / expired / unknown).
fn render_device_message(title: &str, body: &str) -> String {
    let title = html_escape(title);
    let body = html_escape(body);
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>music gateway · {title}</title>
<style>
{DEVICE_PAGE_CSS}
</style>
</head>
<body>
<h1>{title}</h1>
<p>{body}</p>
</body>
</html>"#
    )
}

const DEVICE_PAGE_CSS: &str =
    "body { font-family: system-ui, sans-serif; max-width: 26rem; margin: 4rem auto; padding: 0 1rem; }";

/// Best-effort public base URL for this request: scheme from
/// `X-Forwarded-Proto` (default `https`, since the gateway is always
/// behind TLS), host from `Host`. Empty string when `Host` is absent —
/// the verification URI then comes out relative (`/oauth/device`), which
/// the CLI resolves against the gateway URL it already knows.
fn request_base_url(headers: &HeaderMap) -> String {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("https");
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if host.is_empty() {
        String::new()
    } else {
        format!("{scheme}://{host}")
    }
}

fn now_unix_ms() -> i64 {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    i64::try_from(ms).unwrap_or(i64::MAX)
}
