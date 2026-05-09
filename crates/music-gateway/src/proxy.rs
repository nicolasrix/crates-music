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
use music_cache::{Entry, etag_for};
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

#[tracing::instrument(
    name = "proxy.subsonic",
    skip_all,
    fields(
        method = tracing::field::Empty,
        path = %request.uri().path(),
        kind = tracing::field::Empty,
    ),
)]
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
    tracing::Span::current().record("method", subsonic_method);
    let client_query = request.uri().query().unwrap_or("");

    if BROWSE_METHODS.contains(&subsonic_method) {
        tracing::Span::current().record("kind", "browse");
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
    tracing::Span::current().record("kind", "stream");

    pass_through(&state, subsonic_method, client_query).await
}

#[tracing::instrument(
    name = "proxy.browse",
    skip_all,
    fields(method = %method, outcome = tracing::field::Empty),
)]
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
            tracing::Span::current().record("outcome", "not_modified");
            return Ok(not_modified(&entry.etag));
        }
        tracing::Span::current().record("outcome", "cache_hit");
        return Ok(serve_from_cache(&entry));
    }
    tracing::Span::current().record("outcome", "upstream_fetch");

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

    // Async cache write: compute the etag inline (cheap — ~µs SHA-256
    // over JSON bodies) and serve the response immediately. The
    // SQLite INSERT (with WAL fsync, ~13–20 ms p99) runs on a tokio
    // task so it doesn't block the request path. Single-user system,
    // so the worst-case race — a second identical request landing in
    // the ~ms window before the write commits — is benign: it just
    // does one redundant upstream fetch.
    let etag = etag_for(&body_bytes);
    let entry = Entry {
        key: key.clone(),
        etag,
        body: body_bytes.clone(),
        fetched_at: now,
        ttl,
    };
    let cache = state.cache().clone();
    tokio::spawn(async move {
        if let Err(e) = cache.put(&key, body_bytes, ttl).await {
            tracing::warn!(error = %e, key = %key, "background cache write failed");
        }
    });
    Ok(serve_from_cache(&entry))
}

// Note: this span captures only the time until the upstream *headers*
// arrive — once we wrap the body stream and return, the span closes.
// The actual byte transfer to the client happens after, untracked.
// Header-time is the right thing to measure for "did Navidrome
// respond fast?"; first-byte-to-listener latency is on the client side
// (`web-vital.TTFB` and `playback.start`).
#[tracing::instrument(
    name = "proxy.stream",
    skip_all,
    fields(method = %method, status = tracing::field::Empty),
)]
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
    tracing::Span::current().record("status", status.as_u16());
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
