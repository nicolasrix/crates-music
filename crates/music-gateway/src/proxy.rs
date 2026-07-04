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

use std::time::{Duration, Instant, SystemTime};

use axum::{
    body::Body,
    extract::{Request, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH},
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
// `seed` is a gateway-only hint for the cover-art placeholder rendering
// (web client passes the album/artist display name); strip it before the
// upstream request so Navidrome doesn't see an unexpected param.
const STRIPPED_PARAM_KEYS: &[&str] = &["u", "p", "t", "s", "v", "c", "f", "seed", "access_token"];

/// Mutating Subsonic methods the `/rest` proxy refuses for **every** role
/// (403, before any upstream call). Rationale (sec review 1.2): the gateway
/// owns the write surface — playlists moved to `/v1/playlists`, ratings/likes
/// to `/v1/library/rating` (with a hard no-Navidrome-writeback rule), and
/// accounts to `/v1/admin/users`. Proxying these to Navidrome under the
/// gateway's *shared* credential (typically a Navidrome admin) would let any
/// authenticated caller — a guest most acutely — mutate shared catalog state
/// or reach Navidrome-admin operations (`createUser`/`changePassword`). No
/// legitimate client calls them through `/rest`, so blocking is zero-regression.
///
/// Compared case-insensitively with any trailing `.view` stripped (see
/// [`is_subsonic_write_method`]). `scrobble` is deliberately absent — it is a
/// separate, intentional route (`/rest/scrobble`) writing Navidrome's
/// canonical play-count ledger.
const SUBSONIC_WRITE_METHODS: &[&str] = &[
    // Ratings / stars (gateway-owned, no writeback).
    "star",
    "unstar",
    "setrating",
    // Playlists (gateway-owned via /v1/playlists).
    "createplaylist",
    "updateplaylist",
    "deleteplaylist",
    // Play queue (gateway sync owns this).
    "saveplayqueue",
    // Sharing.
    "createshare",
    "updateshare",
    "deleteshare",
    // Navidrome user administration — must never be reachable via the proxy.
    "createuser",
    "updateuser",
    "deleteuser",
    "changepassword",
    // Internet radio.
    "createinternetradiostation",
    "updateinternetradiostation",
    "deleteinternetradiostation",
    // Podcasts.
    "createpodcastchannel",
    "deletepodcastchannel",
    "deletepodcastepisode",
    "downloadpodcastepisode",
    "refreshpodcasts",
    // Bookmarks.
    "createbookmark",
    "deletebookmark",
    // Library scan + jukebox control.
    "startscan",
    "jukeboxcontrol",
];

/// `true` iff `method` is a mutating Subsonic method the proxy blocks. The
/// comparison strips a legacy `.view` suffix and lower-cases, so `star`,
/// `star.view`, and `STAR` all match — Navidrome's routing tolerates case
/// and the `.view` form, so the guard must too, or it's trivially bypassed.
fn is_subsonic_write_method(method: &str) -> bool {
    let lower = method.to_ascii_lowercase();
    let base = lower.strip_suffix(".view").unwrap_or(&lower);
    SUBSONIC_WRITE_METHODS.contains(&base)
}

/// Subsonic methods we cache. Catalog browse only — playback / mutating /
/// session endpoints stay pass-through.
const BROWSE_METHODS: &[&str] = &[
    "getAlbumList2",
    "getAlbum",
    "getArtists",
    "getArtist",
    "search3",
];

/// Cover-art TTL. 30 days — covers don't change often, and Navidrome's
/// `coverArt` IDs are themselves content-addressed: when a cover file
/// changes, the ID changes too, so invalidation is implicit.
const COVER_ART_TTL_SECONDS: u64 = 30 * 24 * 60 * 60;

/// How many distinct cover-art ids must share the same body etag before
/// the duplicate-classifier flags it as Navidrome's hardcoded
/// placeholder. Threshold-of-1 false-positives on legitimate cover
/// sharing — multi-disc box sets, deluxe editions, soundtrack
/// compilations all routinely re-use the same artwork bytes across
/// distinct cover-art ids. The Navidrome default placeholder, by
/// contrast, appears under *every* art-less album, easily dozens.
/// Five is the smallest threshold that covers realistic legitimate
/// sharing while still detecting the default in any non-trivial
/// library.
const PLACEHOLDER_DUPLICATE_THRESHOLD: usize = 5;

/// Cooldown between background revalidations for the same cache key.
/// A placeholder cache hit fires a re-check at most once every 15 min
/// per (id, size) pair — so a page reload with 60 covers issues at most
/// 60 upstream fetches, and refreshing repeatedly within the window
/// adds none. 15 min is short enough that fixing metadata in Navidrome
/// gets reflected in the gateway within a coffee break.
const PLACEHOLDER_REVALIDATION_COOLDOWN: Duration = Duration::from_mins(15);

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

    // Read-only proxy: refuse mutating Subsonic methods for every role
    // before touching upstream (sec review 1.2). These are gateway-owned
    // or Navidrome-admin operations no legitimate client proxies.
    if is_subsonic_write_method(subsonic_method) {
        tracing::Span::current().record("kind", "write_blocked");
        tracing::warn!(method = %subsonic_method, "blocked mutating subsonic method on read-only proxy");
        return Ok(subsonic_forbidden());
    }

    let client_query = request.uri().query().unwrap_or("");

    if BROWSE_METHODS.contains(&subsonic_method) && is_cacheable(subsonic_method, client_query) {
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

    if subsonic_method == "getCoverArt" {
        tracing::Span::current().record("kind", "cover_art");
        let if_none_match = request
            .headers()
            .get(IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        return cover_art_proxy(&state, client_query, if_none_match.as_deref()).await;
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
    let upstream_url =
        build_upstream_url(state.config(), method, client_query).map_err(|e| e.status())?;
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

/// `/rest/getCoverArt` proxy with an aggressive long-TTL cache and a
/// deterministic SVG fallback for missing covers.
///
/// Three reasons this can't share `browse_proxy`:
///
///   1) The body is binary, not JSON, and the response carries an
///      explicit `Content-Type` from upstream that we must preserve.
///   2) Upstream `404` is the common-case "no cover for this id" — we
///      rewrite it to a `200 image/svg+xml` placeholder so the browser
///      never sees a broken image.
///   3) TTL is much longer (covers rarely change at our scale, and
///      Navidrome's `coverArt` IDs are content-derived — when the file
///      changes, the id changes, so the old cache entry becomes
///      unreachable rather than stale).
// One cohesive request flow (cache hit/ETag revalidation/upstream fetch/
// placeholder fallback); splitting it would scatter the shared `key`/`ttl`/
// `now` context without making it clearer. 24 lines over the pedantic cap.
#[allow(clippy::too_many_lines)]
#[tracing::instrument(
    name = "proxy.cover_art",
    skip_all,
    fields(outcome = tracing::field::Empty),
)]
async fn cover_art_proxy(
    state: &AppState,
    client_query: &str,
    if_none_match: Option<&str>,
) -> Result<Response, StatusCode> {
    let key = cover_art_cache_key(client_query);
    let ttl = Duration::from_secs(COVER_ART_TTL_SECONDS);
    let now = SystemTime::now();
    let cover_id = parse_cover_art_id(client_query).unwrap_or_default();
    let placeholder_seed = parse_cover_art_seed(client_query).unwrap_or_else(|| cover_id.clone());

    if let Some(entry) = state
        .cache()
        .get(&key)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        && entry.is_fresh(now)
    {
        // Re-render previously-cached placeholder SVGs against the
        // current request's seed. An entry can be in cache from before
        // the web client started sending `?seed=<display-name>`: in
        // that era we fell back to the cover-art id as seed, which
        // always starts with "al-…" / "ar-…" — first letter "A" for
        // every album and artist. Detect by SVG magic bytes (real
        // covers are never SVG-prefixed) and re-render with the now-
        // available real seed.
        if is_our_placeholder_body(&entry.body) {
            tracing::Span::current().record("outcome", "placeholder_rerender");
            // Self-heal upward: in the background, see if upstream now
            // has real art for this id (e.g. user filled metadata in
            // Navidrome). Cooldown-gated so a burst of cache hits
            // doesn't translate to a burst of upstream requests.
            maybe_spawn_revalidation(state, &key, client_query);
            return Ok(write_and_serve_placeholder(
                state,
                &key,
                &placeholder_seed,
                ttl,
                now,
            ));
        }
        // Run the placeholder classifier on the cache hit too. The
        // common path here is the second-visit-after-cold-cache
        // scenario: 60 album covers fetched in parallel on first load
        // raced past the SQL duplicate-check (no entries committed
        // yet), all cached as raw upstream bodies. Without classifying
        // on cache hits, the user would keep seeing Navidrome's
        // default forever.
        if classify_as_placeholder(state, &entry.etag, &cover_id).await {
            tracing::Span::current().record("outcome", "placeholder_cache_hit");
            // Same self-heal hook as the SVG branch — once classifier
            // recognises an entry as a Navidrome default, we want to
            // re-check whether real art has landed since.
            maybe_spawn_revalidation(state, &key, client_query);
            return Ok(write_and_serve_placeholder(
                state,
                &key,
                &placeholder_seed,
                ttl,
                now,
            ));
        }
        if let Some(client_etag) = if_none_match
            && client_etag == entry.etag
        {
            tracing::Span::current().record("outcome", "not_modified");
            return Ok(not_modified(&entry.etag));
        }
        tracing::Span::current().record("outcome", "cache_hit");
        return Ok(serve_cover_from_cache(&entry));
    }

    let upstream_url = build_upstream_url(state.config(), "getCoverArt", client_query)
        .map_err(|e| e.status())?;
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

    // 404 → cache the deterministic SVG so subsequent requests are served
    // straight from disk without re-pinging Navidrome. Two ?size= variants
    // for the same missing id render visually identical placeholders
    // (still keyed separately in the cache so the first 200-px request
    // doesn't pin future 600-px requests once Navidrome eventually gets
    // a real cover).
    if status == StatusCode::NOT_FOUND {
        tracing::Span::current().record("outcome", "placeholder_404");
        return Ok(write_and_serve_placeholder(
            state,
            &key,
            &placeholder_seed,
            ttl,
            now,
        ));
    }

    if !status.is_success() {
        // 5xx and other transient errors: forward verbatim and DO NOT
        // cache. Caching an error sentinel here would mask a Navidrome
        // recovery — next request must re-hit upstream.
        tracing::Span::current().record("outcome", "upstream_error");
        return Ok(forward_buffered(status, &upstream_headers, body_bytes));
    }

    // Subsonic returns "no artwork" as 200 + application/json with an
    // error envelope, not 404. Treat the same as a 404: serve SVG, do
    // not cache the JSON bytes.
    if is_subsonic_json_error(&upstream_headers, &body_bytes) {
        tracing::Span::current().record("outcome", "placeholder_subsonic_error");
        return Ok(write_and_serve_placeholder(
            state,
            &key,
            &placeholder_seed,
            ttl,
            now,
        ));
    }

    let etag = etag_for(&body_bytes);

    // Fast-path: did a previous request already register this etag as
    // Navidrome's placeholder? If so, skip the SQL roundtrip.
    let already_known = state
        .placeholder_etags()
        .read()
        .is_ok_and(|set| set.contains(&etag));
    if already_known {
        tracing::Span::current().record("outcome", "placeholder_known");
        return Ok(write_and_serve_placeholder(
            state,
            &key,
            &placeholder_seed,
            ttl,
            now,
        ));
    }

    // Synchronous cache write — *not* `tokio::spawn`. Without this,
    // 60 concurrent first-page-load requests all race past the
    // `keys_with_etag` SQL check before any has committed, so none
    // detect on the first round. With a synchronous write, the SQLite
    // writer lock serialises the commits; by the time request K's
    // duplicate-check SQL runs, requests 1..K-1 are visible. The cost
    // is ~13–20 ms p99 per cold cover-art fetch, paid only on cache
    // miss; subsequent visits hit the cache and skip this entirely.
    let entry = Entry {
        key: key.clone(),
        etag: etag.clone(),
        body: body_bytes.clone(),
        fetched_at: now,
        ttl,
    };
    if let Err(e) = state.cache().put(&key, body_bytes.clone(), ttl).await {
        tracing::warn!(error = %e, key = %key, "cover-art cache write failed");
    }

    // Now classify. The just-written entry plus any concurrently-committed
    // peers under different cover-art ids are visible to the SQL query —
    // detection fires on the first parallel round, not the second.
    if classify_as_placeholder(state, &etag, &cover_id).await {
        tracing::Span::current().record("outcome", "placeholder_detected");
        return Ok(write_and_serve_placeholder(
            state,
            &key,
            &placeholder_seed,
            ttl,
            now,
        ));
    }

    tracing::Span::current().record("outcome", "upstream_fetch");
    let content_type = upstream_headers
        .get(CONTENT_TYPE)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("application/octet-stream"));
    Ok(serve_cover_from_cache_with_ct(&entry, &content_type))
}

/// Decide whether a body identified by `etag` is one of Navidrome's
/// default placeholder images. The result has side effects: when a new
/// placeholder is identified, its etag is registered in the in-memory
/// set (so future requests skip the SQL check) and a sweep is queued
/// to flush already-cached copies under other ids.
///
/// Called from both the cache-hit and the cold-fetch paths; centralised
/// here so the two paths stay in sync.
async fn classify_as_placeholder(state: &AppState, etag: &str, current_cover_id: &str) -> bool {
    let already_known = state
        .placeholder_etags()
        .read()
        .is_ok_and(|set| set.contains(etag));
    if already_known {
        return true;
    }
    let is_duplicate = is_duplicate_cover_etag(state.cache(), etag, current_cover_id)
        .await
        .unwrap_or(false);
    if !is_duplicate {
        return false;
    }
    if let Ok(mut set) = state.placeholder_etags().write() {
        set.insert(etag.to_string());
    }
    // Fire-and-forget sweep. Idempotent — multiple concurrent
    // detections all converge on the same end state (no entries with
    // this etag remain).
    let cache_for_sweep = state.cache().clone();
    let etag_for_sweep = etag.to_string();
    tokio::spawn(async move {
        if let Err(e) = cache_for_sweep.delete_keys_with_etag(&etag_for_sweep).await {
            tracing::warn!(error = %e, etag = %etag_for_sweep, "placeholder sweep failed");
        }
    });
    true
}

/// Spawn a background task that re-fetches upstream for `key` and
/// replaces the cached placeholder if upstream now serves real art —
/// but only if no other revalidation has run for this key within the
/// cooldown window. Idempotent under concurrent calls (the cooldown
/// map is a single mutex; only the first caller within a window
/// inserts and spawns).
fn maybe_spawn_revalidation(state: &AppState, key: &str, client_query: &str) {
    let now = Instant::now();
    {
        // Poisoned mutex from a prior panic skips this attempt rather
        // than taking revalidation offline forever; next request retries.
        let Ok(mut map) = state.placeholder_revalidations().lock() else {
            return;
        };
        if let Some(last) = map.get(key)
            && now.duration_since(*last) < PLACEHOLDER_REVALIDATION_COOLDOWN
        {
            return;
        }
        map.insert(key.to_string(), now);
    }

    let state = state.clone();
    let key = key.to_string();
    let client_query = client_query.to_string();
    tokio::spawn(async move {
        revalidate_placeholder(&state, &key, &client_query).await;
    });
}

/// Background half of placeholder revalidation. Fetches upstream and,
/// if the response looks like real art (not a known placeholder, not
/// duplicating any other id's body, not an error), replaces the cached
/// SVG with the upstream bytes. Conservative: any signal that this
/// might still be a placeholder leaves the cached SVG in place.
///
/// Every early-return path emits a `tracing::debug!` line with the
/// reason, so production silence on a stuck SVG can be diagnosed by
/// dropping the log level rather than rebuilding with extra logs.
async fn revalidate_placeholder(state: &AppState, key: &str, client_query: &str) {
    let Ok(upstream_url) = build_upstream_url(state.config(), "getCoverArt", client_query) else {
        tracing::debug!(key = %key, "revalidation skip: upstream URL build failed");
        return;
    };
    let response = match state.http().get(upstream_url).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(error = %e, key = %key, "revalidation skip: upstream unreachable");
            return;
        }
    };
    let status = response.status();
    if status != StatusCode::OK {
        // 404: still missing. 5xx: transient. Either way, no replacement.
        tracing::debug!(key = %key, status = %status, "revalidation skip: non-200 upstream");
        return;
    }
    // Header snapshot must be taken before consuming the body.
    let upstream_headers = response.headers().clone();
    let Ok(body) = response.bytes().await else {
        tracing::debug!(key = %key, "revalidation skip: body read failed");
        return;
    };
    if body.is_empty() {
        tracing::debug!(key = %key, "revalidation skip: empty body");
        return;
    }
    // Subsonic JSON-error envelope (e.g. stale cover-art id after a
    // re-tag). Same protection as the cold path: don't replace the SVG,
    // don't poison the cache.
    if is_subsonic_json_error(&upstream_headers, &body) {
        tracing::debug!(key = %key, "revalidation skip: subsonic json-error");
        return;
    }

    let etag = etag_for(&body);

    // Already-known placeholder etag — fast bail without an SQL roundtrip.
    if state
        .placeholder_etags()
        .read()
        .is_ok_and(|s| s.contains(&etag))
    {
        tracing::debug!(key = %key, etag = %etag, "revalidation skip: known-placeholder etag");
        return;
    }

    // Newly-discovered placeholder via the duplicate check: register
    // and skip the write. We must check duplicates *before* writing —
    // writing first would briefly replace our SVG with the placeholder
    // bytes between the put and the next request's cache-hit classify.
    let cover_id = cover_id_from_cache_key(key).unwrap_or_default().to_string();
    if is_duplicate_cover_etag(state.cache(), &etag, &cover_id)
        .await
        .unwrap_or(false)
    {
        if let Ok(mut s) = state.placeholder_etags().write() {
            s.insert(etag.clone());
        }
        tracing::debug!(key = %key, etag = %etag, "revalidation skip: duplicate-classifier matched");
        return;
    }

    // Looks like real art. Replace the cached SVG.
    let ttl = Duration::from_secs(COVER_ART_TTL_SECONDS);
    if let Err(e) = state.cache().put(key, body, ttl).await {
        tracing::warn!(error = %e, key = %key, "placeholder revalidation: cache write failed");
    } else {
        tracing::info!(key = %key, "placeholder revalidation: replaced with upstream art");
    }
}

/// Build, cache, and serve the deterministic SVG placeholder for the
/// given seed. Used by the 404 branch and both placeholder-detection
/// branches in `cover_art_proxy`.
fn write_and_serve_placeholder(
    state: &AppState,
    key: &str,
    seed: &str,
    ttl: Duration,
    now: SystemTime,
) -> Response {
    let svg = render_placeholder_svg(seed);
    let body = Bytes::from(svg.into_bytes());
    let etag = etag_for(&body);
    let entry = Entry {
        key: key.to_string(),
        etag,
        body: body.clone(),
        fetched_at: now,
        ttl,
    };
    let cache = state.cache().clone();
    let key_for_task = key.to_string();
    let body_for_task = body;
    tokio::spawn(async move {
        if let Err(e) = cache.put(&key_for_task, body_for_task, ttl).await {
            tracing::warn!(error = %e, key = %key_for_task, "cover-art placeholder write failed");
        }
    });
    // Placeholder responses use `no-cache, must-revalidate` (rather than
    // real art's `max-age=300`). Reason: a placeholder may flip to real
    // art at any moment — e.g. Navidrome ingests the cover file, or the
    // gateway's background revalidation populates the cache. Without
    // forced revalidation, a browser that fetched the placeholder once
    // would keep showing it for 5 min even after real art is available
    // server-side. `no-cache` here means "always revalidate"; the etag
    // makes the steady-state cost a cheap 304.
    serve_cover_from_cache_with_cc(
        &entry,
        &guess_image_content_type(&entry.body),
        HeaderValue::from_static("no-cache, must-revalidate"),
    )
}

/// `true` iff at least [`PLACEHOLDER_DUPLICATE_THRESHOLD`] cover-art
/// ids — distinct from `current_cover_id` — share this etag. That's
/// the signature of Navidrome's hardcoded default placeholder
/// (byte-identical for every art-less album); legitimate cover sharing
/// (multi-disc, deluxe edition, compilation re-use) stays under the
/// threshold and is left alone.
///
/// Only entity-level IDs (`al-*`, `ar-*`) count toward the threshold.
/// Track-level IDs (`mf-*`) are excluded because every track in an
/// album resolves to the same album cover bytes — an album with 15
/// tracks produces 15 entries with identical etags, which would
/// false-positive as the Navidrome default without this filter.
///
/// Different sizes of the same id (`id=X&size=200` and `id=X&size=600`
/// when Navidrome doesn't re-thumbnail) are filtered out — those are
/// the same album, not duplication.
async fn is_duplicate_cover_etag(
    cache: &music_cache::Cache,
    etag: &str,
    current_cover_id: &str,
) -> Result<bool, music_cache::Error> {
    let keys = cache.keys_with_etag(etag).await?;
    let distinct_other_ids: std::collections::HashSet<&str> = keys
        .iter()
        .filter_map(|k| cover_id_from_cache_key(k))
        .filter(|id| *id != current_cover_id)
        .filter(|id| !id.starts_with("mf-"))
        .collect();
    Ok(distinct_other_ids.len() >= PLACEHOLDER_DUPLICATE_THRESHOLD)
}

/// `true` when the cached body is one we generated ourselves (an SVG
/// placeholder). Real album/artist covers are JPEG/PNG/WebP — none of
/// those start with `<?xml` or `<svg`. Used to re-render stale SVGs
/// against the current request's seed without re-hitting upstream.
fn is_our_placeholder_body(body: &[u8]) -> bool {
    body.starts_with(b"<?xml") || body.starts_with(b"<svg")
}

/// `true` when the upstream response is Subsonic's "no artwork"
/// envelope (HTTP 200 + `application/json` + `status:"failed"`). The
/// common trigger is a stale `coverArt` id: after the user re-tags
/// metadata in Navidrome the underlying file's content/mtime hash
/// changes, so the previous id no longer resolves. Subsonic surfaces
/// that as a JSON error envelope rather than a 404, which would
/// otherwise sneak past the success-status check and get cached as
/// "image bytes". We treat it the same as a 404 — substitute SVG,
/// don't poison the cache.
fn is_subsonic_json_error(headers: &HeaderMap, body: &[u8]) -> bool {
    let Some(ct) = headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    if !ct.starts_with("application/json") {
        return false;
    }
    let Ok(s) = std::str::from_utf8(body) else {
        return false;
    };
    // Cheap substring scan over a body that's reliably <500 bytes —
    // avoids a serde dep just to check two fields.
    s.contains(r#""subsonic-response""#) && s.contains(r#""status":"failed""#)
}

/// Inverse of [`cover_art_cache_key`]: pull the `id=…` segment out of a
/// cached cover-art key. Returns `None` for non-cover-art keys.
fn cover_id_from_cache_key(k: &str) -> Option<&str> {
    let rest = k.strip_prefix("getCoverArt|id=")?;
    Some(rest.split('|').next().unwrap_or(rest))
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
    let upstream_url =
        build_upstream_url(state.config(), method, client_query).map_err(|e| e.status())?;
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

/// 403 for a blocked mutating method, shaped as a Subsonic error envelope
/// (code 50 = "user not authorized for the given operation") so a Subsonic
/// client gets a coherent failure rather than an opaque empty body.
fn subsonic_forbidden() -> Response {
    let body = Bytes::from_static(
        br#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":50,"message":"This operation is not permitted through the gateway proxy."}}}"#,
    );
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = StatusCode::FORBIDDEN;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
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

/// Cover-art response shared by cache-hit and cold-fetch paths. SVG
/// placeholders go through the same path with `image/svg+xml` so the
/// downstream behaviour (etag, cache-control, body shape) is identical.
fn serve_cover_from_cache(entry: &Entry) -> Response {
    let ct = guess_image_content_type(&entry.body);
    serve_cover_from_cache_with_ct(entry, &ct)
}

fn serve_cover_from_cache_with_ct(entry: &Entry, content_type: &HeaderValue) -> Response {
    // Real-art default: 5-min freshness, then etag revalidate.
    serve_cover_from_cache_with_cc(
        entry,
        content_type,
        HeaderValue::from_static("public, max-age=300, must-revalidate"),
    )
}

fn serve_cover_from_cache_with_cc(
    entry: &Entry,
    content_type: &HeaderValue,
    cache_control: HeaderValue,
) -> Response {
    let mut response = Response::new(Body::from(entry.body.clone()));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&entry.etag) {
        headers.insert(ETAG, value);
    }
    headers.insert(CONTENT_TYPE, content_type.clone());
    // We previously sent `immutable` for everything on the assumption
    // that Navidrome's coverArt ids are content-derived (and they are
    // for real artwork — file change → id change). But the same id can
    // flip from "Navidrome default placeholder" to "real cover" once
    // the user uploads art, and the gateway can also retroactively
    // rewrite cached bodies once placeholder detection fires on later
    // requests. Both transitions must be observable through the browser
    // cache. Callers pick the directive: real art uses `max-age=300`;
    // placeholders use `no-cache` so the flip-to-real-art is observed
    // promptly. ETag means the steady-state cost is a cheap 304 either way.
    headers.insert(CACHE_CONTROL, cache_control);
    response
}

/// The L2 cache stores raw bytes — when we serve from cache without a
/// remembered upstream content-type, sniff the magic bytes for the few
/// formats Navidrome actually returns (PNG, JPEG, WebP) and our own SVG
/// fallback. Conservative: anything unrecognised becomes
/// `application/octet-stream` rather than guessing.
fn guess_image_content_type(body: &[u8]) -> HeaderValue {
    if body.starts_with(b"\x89PNG\r\n\x1a\n") {
        HeaderValue::from_static("image/png")
    } else if body.starts_with(&[0xFF, 0xD8, 0xFF]) {
        HeaderValue::from_static("image/jpeg")
    } else if body.len() >= 12 && &body[0..4] == b"RIFF" && &body[8..12] == b"WEBP" {
        HeaderValue::from_static("image/webp")
    } else if body.starts_with(b"<svg") || body.starts_with(b"<?xml") {
        HeaderValue::from_static("image/svg+xml; charset=utf-8")
    } else {
        HeaderValue::from_static("application/octet-stream")
    }
}

/// Deterministic SVG placeholder for missing covers. Mirrors the web
/// client's `<Cover />` placeholder algorithm so the visual language is
/// consistent across proxy-served fallbacks and client-rendered ones —
/// both pick a hue from FNV-1a(seed) × golden-angle. The seed is the
/// Subsonic cover-art id (deliberately not the album/artist name —
/// names aren't reachable from the proxy without an extra Navidrome
/// round trip, and the id is stable enough for a placeholder).
fn render_placeholder_svg(seed: &str) -> String {
    let initial = first_initial(seed);
    let hash = fnv1a32(if seed.is_empty() { "?" } else { seed });
    // f64 multiplication then modulo keeps the spread good for the
    // 32-bit hash space without losing precision.
    let hue = ((f64::from(hash) * 137.508_f64) % 360.0).abs();
    let hue1 = format!("{hue:.1}");
    let hue2 = format!("{:.1}", (hue + 28.0) % 360.0);
    // Ids go into both an `<linearGradient id>` and an `xlink:href`/`fill`
    // — they need to be stable per body but never collide with another
    // SVG on the same page. We don't have document-wide context here
    // (the placeholder is a standalone SVG document), so a fixed id is
    // fine.
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" preserveAspectRatio="xMidYMid slice" role="img" aria-hidden="true"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0%" stop-color="hsl({hue1}, 55%, 38%)"/><stop offset="100%" stop-color="hsl({hue2}, 55%, 22%)"/></linearGradient></defs><rect width="100" height="100" fill="url(#g)"/><text x="50" y="50" text-anchor="middle" dominant-baseline="central" fill="rgba(255,255,255,0.92)" font-family="system-ui, -apple-system, Segoe UI, Roboto, sans-serif" font-weight="700" font-size="44" dy=".05em">{initial}</text></svg>"#
    )
}

fn first_initial(seed: &str) -> String {
    let Some(c) = seed.chars().find(|c| !c.is_whitespace()) else {
        return "?".to_string();
    };
    let mut buf = String::new();
    for upper in c.to_uppercase() {
        // XML-escape the rare reserved characters that could land here
        // from arbitrary cover-art ids ("<", "&", etc.).
        match upper {
            '<' => buf.push_str("&lt;"),
            '>' => buf.push_str("&gt;"),
            '&' => buf.push_str("&amp;"),
            other => buf.push(other),
        }
    }
    buf
}

/// FNV-1a 32-bit. Same algorithm as the web client's `hashSeed` so the
/// placeholder hue picked here matches what `<Cover />` would render
/// for the same seed — useful when we serve a real cover later and the
/// hue jump becomes a "this is the same item, just got artwork" signal.
fn fnv1a32(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for byte in s.as_bytes() {
        h ^= u32::from(*byte);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

fn forward_buffered(status: StatusCode, headers: &HeaderMap, body: Bytes) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    if let Some(ct) = headers.get(CONTENT_TYPE).cloned() {
        response.headers_mut().insert(CONTENT_TYPE, ct);
    }
    response
}

/// Whether a browse-shaped request can safely be served from the L2 cache.
///
/// `getAlbumList2?type=random` is the only entry in `BROWSE_METHODS` whose
/// response isn't a pure function of catalog state — Navidrome shuffles
/// server-side per call. Caching it would pin the first shuffle for
/// `browse_ttl_seconds` and the random-browse pages would show the same
/// list every visit. Every other `type` (newest, frequent, alphabeticalByName,
/// …) is deterministic at single-user scale and benefits from the cache.
fn is_cacheable(method: &str, client_query: &str) -> bool {
    if method != "getAlbumList2" {
        return true;
    }
    let is_random = url::form_urlencoded::parse(client_query.as_bytes())
        .any(|(k, v)| k == "type" && v == "random");
    !is_random
}

/// Cover-art cache key. Like `cache_key` but limited to the small set of
/// query params that actually shape the response (`id`, `size`) — Subsonic
/// has historically allowed a few cover-art-only flags but we only honour
/// these two. Including `id` and `size` separately means a 200-px and
/// 600-px request for the same id are independent cache entries (so a
/// thumbnail can't mask a larger detail-page cover).
fn cover_art_cache_key(client_query: &str) -> String {
    let mut id = String::new();
    let mut size = String::new();
    for (k, v) in url::form_urlencoded::parse(client_query.as_bytes()) {
        match k.as_ref() {
            "id" => id = v.into_owned(),
            "size" => size = v.into_owned(),
            _ => {}
        }
    }
    format!("getCoverArt|id={id}|size={size}")
}

fn parse_cover_art_id(client_query: &str) -> Option<String> {
    url::form_urlencoded::parse(client_query.as_bytes())
        .find(|(k, _)| k == "id")
        .map(|(_, v)| v.into_owned())
}

/// Optional `seed` query param: a display name (album title, artist
/// name) the web client passes so the placeholder shows the right
/// initial ("P" for Pink Floyd) rather than the cover-art id's first
/// letter ("A" for "ar-…"). Empty values are treated as absent.
fn parse_cover_art_seed(client_query: &str) -> Option<String> {
    url::form_urlencoded::parse(client_query.as_bytes())
        .find(|(k, _)| k == "seed")
        .map(|(_, v)| v.into_owned())
        .filter(|s| !s.is_empty())
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

/// Why building an upstream URL failed.
#[derive(Debug)]
enum UpstreamUrlError {
    /// The Subsonic method name contained path-navigation characters
    /// (`/`, `\`, or a `..` segment). `Url::join` interprets those as
    /// relative-path navigation, so a crafted method like `../admin`
    /// would escape the `rest/` prefix and reach an arbitrary upstream
    /// endpoint — with the gateway's Navidrome credentials attached.
    /// Rejected before the join. A caller bug / hostile request, so the
    /// handlers surface it as 400, not 500.
    InvalidMethod,
    /// The configured upstream base URL didn't parse. A server
    /// misconfiguration → 500.
    Parse,
}

impl UpstreamUrlError {
    fn status(&self) -> StatusCode {
        match self {
            UpstreamUrlError::InvalidMethod => StatusCode::BAD_REQUEST,
            UpstreamUrlError::Parse => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// A Subsonic method name is safe to splice into the upstream path iff it
/// carries no path-navigation. Real method names are simple identifiers
/// (`getAlbumList2`, `stream`), optionally with the legacy `.view` suffix
/// (`ping.view`) — none contain a separator or a `..` segment. We block
/// `/`, `\`, and the `..` substring rather than all dots so the `.view`
/// form keeps proxying. Percent-encoded separators (`%2f`, `%2e`) are
/// preserved verbatim by `Url::join` and never act as navigation, so a
/// literal-character check is sufficient to keep the join inside `rest/`.
fn is_valid_subsonic_method(method: &str) -> bool {
    !method.is_empty()
        && !method.contains('/')
        && !method.contains('\\')
        && !method.contains("..")
}

fn build_upstream_url(
    config: &Config,
    subsonic_method: &str,
    client_query: &str,
) -> Result<Url, UpstreamUrlError> {
    if !is_valid_subsonic_method(subsonic_method) {
        return Err(UpstreamUrlError::InvalidMethod);
    }
    let base = ensure_trailing_slash(&config.upstream.navidrome_url);
    let mut url = Url::parse(&base)
        .map_err(|_| UpstreamUrlError::Parse)?
        .join(&format!("rest/{subsonic_method}"))
        .map_err(|_| UpstreamUrlError::Parse)?;

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
        // Do not follow redirects. This client talks only to the trusted
        // upstream Navidrome with the gateway's credentials attached to
        // every request. If a (compromised or misconfigured) upstream
        // answered with a 3xx to an attacker-controlled host, reqwest's
        // default policy would replay the request — and its query string,
        // which carries `u`/`t`/`s` auth params — to that location.
        // Surfacing the 3xx to the client instead keeps the credentials
        // from ever leaving the gateway↔Navidrome hop.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("rustls reqwest client should always build")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn valid_methods_accepted() {
        for m in [
            "ping",
            "stream",
            "getAlbumList2",
            "search3",
            "getCoverArt",
            // Legacy Subsonic `.view` suffix must keep proxying.
            "ping.view",
            "stream.view",
        ] {
            assert!(is_valid_subsonic_method(m), "should accept {m:?}");
        }
    }

    #[test]
    fn write_methods_detected_case_and_view_insensitive() {
        for m in [
            "star",
            "unstar",
            "setRating",
            "createPlaylist",
            "updatePlaylist",
            "deletePlaylist",
            "createUser",
            "deleteUser",
            "changePassword",
            "savePlayQueue",
            "startScan",
            "jukeboxControl",
            // legacy `.view` suffix + case variants must still match
            "star.view",
            "STAR",
            "SetRating.View",
        ] {
            assert!(is_subsonic_write_method(m), "should block {m:?}");
        }
    }

    #[test]
    fn read_methods_not_flagged_as_writes() {
        for m in [
            "ping",
            "stream",
            "getAlbumList2",
            "getAlbum",
            "search3",
            "getCoverArt",
            "getSong",
            "getPlayQueue", // read counterpart of the blocked savePlayQueue
            "scrobble",     // intentional separate route — never blocked here
            "getStarred",   // reads stars; only the mutating star/unstar are blocked
        ] {
            assert!(!is_subsonic_write_method(m), "should allow {m:?}");
        }
    }

    #[test]
    fn path_navigation_methods_rejected() {
        for m in [
            "",
            "../admin",
            "..%2fadmin", // literal `..` still present pre-decode
            "foo/bar",
            "a\\b",
            "..",
            "rest/../admin",
        ] {
            assert!(!is_valid_subsonic_method(m), "should reject {m:?}");
        }
    }

    fn test_config() -> Config {
        Config::from_toml_str(
            r#"
[server]
listen = "0.0.0.0:8443"
tls_cert = "/c.pem"
tls_key = "/k.pem"
bearer_token = "t"

[upstream]
navidrome_url = "http://nav.lan:4533"
username = "alice"
password = "wonderland"

[cache]
path = "/tmp/cache.sqlite"
browse_ttl_seconds = 3600
"#,
        )
        .unwrap()
    }

    #[test]
    fn build_upstream_url_rejects_traversal_with_400() {
        let cfg = test_config();
        let err = build_upstream_url(&cfg, "../admin", "").unwrap_err();
        assert!(matches!(err, UpstreamUrlError::InvalidMethod));
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn build_upstream_url_keeps_valid_method_under_rest_prefix() {
        let cfg = test_config();
        let url = build_upstream_url(&cfg, "getAlbumList2", "type=newest").unwrap();
        // The method stays a single segment under `/rest/` — no escape.
        assert_eq!(url.path(), "/rest/getAlbumList2");
        // Injected auth params are present and the client query rides along.
        let q = url.query().unwrap();
        assert!(q.contains("u=alice"));
        assert!(q.contains("type=newest"));
    }
}
