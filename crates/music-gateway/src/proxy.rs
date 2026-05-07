//! Subsonic /rest/* proxy with optional L2 cache for browse endpoints.
//!
//! Two paths through this module:
//!
//! - **Browse endpoints** (`getAlbumList2`, `getAlbum`, `getArtists`,
//!   `getArtist`, `search3`) are buffered, cached in [`music_cache::Cache`]
//!   under a normalised key, and served with an `ETag` header. Subsequent
//!   requests with a matching `If-None-Match` get `304 Not Modified`.
//!
//! - **Everything else** (`/rest/ping`, `/rest/stream`, …) is streamed
//!   straight through with auth params injected — no buffering, no caching.

use std::time::{Duration, SystemTime};

use axum::{
    body::Body,
    extract::{Request, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CONTENT_TYPE, ETAG, IF_NONE_MATCH},
    },
    response::Response,
};
use bytes::Bytes;
use music_cache::Entry;
use music_subsonic::auth;
use url::Url;

use crate::config::Config;
use crate::state::AppState;

const SUBSONIC_PROTOCOL_VERSION: &str = "1.16.1";
const GATEWAY_CLIENT_NAME: &str = "crates-music-gateway";
const STRIPPED_PARAM_KEYS: &[&str] = &["u", "p", "t", "s", "v", "c", "f"];

/// Subsonic methods we cache. Catalog browse only — playback / mutating /
/// session endpoints stay pass-through.
const BROWSE_METHODS: &[&str] = &[
    "getAlbumList2",
    "getAlbum",
    "getArtists",
    "getArtist",
    "search3",
];

pub async fn proxy(state: State<AppState>, request: Request) -> Response {
    match proxy_inner(state, request).await {
        Ok(response) => response,
        Err(status) => Response::builder()
            .status(status)
            .body(Body::empty())
            .expect("status-only response is well-formed"),
    }
}

async fn proxy_inner(
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, StatusCode> {
    let path = request.uri().path();
    let Some(subsonic_method) = path.strip_prefix("/rest/") else {
        return Err(StatusCode::NOT_FOUND);
    };
    if subsonic_method.is_empty() {
        return Err(StatusCode::NOT_FOUND);
    }
    let client_query = request.uri().query().unwrap_or("");

    if BROWSE_METHODS.contains(&subsonic_method) {
        let if_none_match = request
            .headers()
            .get(IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        return browse_proxy(
            &state,
            subsonic_method,
            client_query,
            if_none_match.as_deref(),
        )
        .await;
    }

    pass_through(&state, subsonic_method, client_query).await
}

async fn browse_proxy(
    state: &AppState,
    method: &str,
    client_query: &str,
    if_none_match: Option<&str>,
) -> Result<Response, StatusCode> {
    let key = cache_key(method, client_query);
    let now = SystemTime::now();
    let ttl = Duration::from_secs(state.config().cache.browse_ttl_seconds);

    if let Some(entry) = state
        .cache()
        .get(&key)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        && entry.is_fresh(now)
    {
        if let Some(client_etag) = if_none_match
            && client_etag == entry.etag
        {
            return Ok(not_modified(&entry.etag));
        }
        return Ok(serve_from_cache(&entry));
    }

    // Miss or stale → fetch upstream, buffer body, store, return.
    let upstream_url = build_upstream_url(state.config(), method, client_query)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let upstream = state
        .http()
        .get(upstream_url)
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    let status = upstream.status();
    let upstream_headers = upstream.headers().clone();
    let body_bytes = upstream
        .bytes()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    if !status.is_success() {
        // Don't poison the cache with errors. Forward verbatim.
        return Ok(forward_buffered(status, &upstream_headers, body_bytes));
    }

    let entry = state
        .cache()
        .put(&key, body_bytes, ttl)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(serve_from_cache(&entry))
}

async fn pass_through(
    state: &AppState,
    method: &str,
    client_query: &str,
) -> Result<Response, StatusCode> {
    let upstream_url = build_upstream_url(state.config(), method, client_query)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let upstream = state
        .http()
        .get(upstream_url)
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    let status = upstream.status();
    let mut downstream_headers = HeaderMap::new();
    if let Some(ct) = upstream.headers().get(CONTENT_TYPE).cloned() {
        downstream_headers.insert(CONTENT_TYPE, ct);
    }
    let stream = upstream.bytes_stream();
    let body = Body::from_stream(stream);

    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = downstream_headers;
    Ok(response)
}

fn not_modified(etag: &str) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NOT_MODIFIED;
    if let Ok(value) = HeaderValue::from_str(etag) {
        response.headers_mut().insert(ETAG, value);
    }
    response
}

fn serve_from_cache(entry: &Entry) -> Response {
    let mut response = Response::new(Body::from(entry.body.clone()));
    *response.status_mut() = StatusCode::OK;
    if let Ok(value) = HeaderValue::from_str(&entry.etag) {
        response.headers_mut().insert(ETAG, value);
    }
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

fn forward_buffered(status: StatusCode, headers: &HeaderMap, body: Bytes) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    if let Some(ct) = headers.get(CONTENT_TYPE).cloned() {
        response.headers_mut().insert(CONTENT_TYPE, ct);
    }
    response
}

/// Cache key derivation: method name plus non-auth query params, sorted by key
/// for deterministic ordering. `?type=newest&size=20` and `?size=20&type=newest`
/// resolve to the same key.
fn cache_key(method: &str, client_query: &str) -> String {
    let mut params: Vec<(String, String)> = url::form_urlencoded::parse(client_query.as_bytes())
        .filter(|(k, _)| !STRIPPED_PARAM_KEYS.contains(&k.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    params.sort();

    let mut key = String::from(method);
    for (k, v) in params {
        key.push('|');
        key.push_str(&k);
        key.push('=');
        key.push_str(&v);
    }
    key
}

fn build_upstream_url(
    config: &Config,
    subsonic_method: &str,
    client_query: &str,
) -> Result<Url, url::ParseError> {
    let base = ensure_trailing_slash(&config.upstream.navidrome_url);
    let mut url = Url::parse(&base)?.join(&format!("rest/{subsonic_method}"))?;

    let salt = auth::random_salt();
    let token = auth::compute_token(&config.upstream.password, &salt);

    {
        let mut q = url.query_pairs_mut();
        q.append_pair("u", &config.upstream.username)
            .append_pair("t", &token)
            .append_pair("s", &salt)
            .append_pair("v", SUBSONIC_PROTOCOL_VERSION)
            .append_pair("c", GATEWAY_CLIENT_NAME)
            .append_pair("f", "json");
        for (key, value) in url::form_urlencoded::parse(client_query.as_bytes()) {
            if STRIPPED_PARAM_KEYS.contains(&key.as_ref()) {
                continue;
            }
            q.append_pair(key.as_ref(), value.as_ref());
        }
    }
    Ok(url)
}

fn ensure_trailing_slash(s: &str) -> String {
    if s.ends_with('/') {
        s.to_string()
    } else {
        format!("{s}/")
    }
}

pub fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .build()
        .expect("rustls reqwest client should always build")
}
