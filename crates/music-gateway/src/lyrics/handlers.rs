//! HTTP surface for lyrics (`/v1/lyrics/*`).
//!
//! Authorization split:
//!   * **Read** (`GET`) is any-authenticated, guests included. Lyrics are
//!     catalog data — the same tier as browsing an album, not per-user
//!     state.
//!   * **Refresh** (`POST`) requires `WriteTaste`, so a guest gets 403.
//!     It re-resolves a *shared* row, so it is a write to state everyone
//!     in the household sees.
//!
//! The response is normalized: whatever the source, a client receives
//! `lines: [{start_ms, text}]` already parsed and sorted. Neither the web
//! player nor the TUI ever sees LRC text.

use std::fmt::Write as _;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use music_core::TrackId;
use music_recommend::{LyricsRow, LyricsSource};

use crate::principal::{AuthPrincipal, Capability, Principal};
use crate::state::AppState;

use super::resolver::ResolveError;

/// How long a client may reuse a lyrics payload without revalidating.
/// Short relative to the server-side TTL: the ETag makes revalidation
/// nearly free, and a refresh should become visible on the next open
/// rather than in six months.
const CLIENT_MAX_AGE_SECONDS: u32 = 300;

/// `GET /v1/lyrics/:track_id` — resolved lyrics, fetching if needed.
pub async fn get_lyrics(
    State(state): State<AppState>,
    AuthPrincipal(_principal): AuthPrincipal,
    Path(track_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, &'static str)> {
    let resolver = state
        .lyrics()
        .ok_or((StatusCode::NOT_FOUND, "lyrics are disabled"))?;
    let track_id = TrackId::from(track_id);
    let row = resolver.resolve(&track_id, false).await.map_err(map_err)?;
    Ok(respond(&row, &headers))
}

/// `POST /v1/lyrics/:track_id/refresh` — drop the cached answer and
/// resolve again. The escape hatch for a bad fuzzy match.
pub async fn refresh_lyrics(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Path(track_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, &'static str)> {
    require_write(&principal)?;
    let resolver = state
        .lyrics()
        .ok_or((StatusCode::NOT_FOUND, "lyrics are disabled"))?;
    let track_id = TrackId::from(track_id);
    // Forget first: a refresh that fails upstream should leave no stale
    // "hit" claiming to be current, and `resolve(force)` will still serve
    // whatever it can find.
    resolver.forget(&track_id).await.map_err(map_err)?;
    let row = resolver.resolve(&track_id, true).await.map_err(map_err)?;
    Ok(respond(&row, &headers))
}

/// Guests may read lyrics but not trigger a shared re-resolution.
fn require_write(principal: &Principal) -> Result<(), (StatusCode, &'static str)> {
    if principal.can(Capability::WriteTaste) {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "guests cannot refresh lyrics"))
    }
}

fn map_err(e: ResolveError) -> (StatusCode, &'static str) {
    match e {
        ResolveError::Disabled => (StatusCode::NOT_FOUND, "lyrics are disabled"),
        // Deliberately *not* 200-with-nothing: "we could not check" and
        // "there are none" must look different to the client, or a
        // transient outage renders as a permanent absence.
        ResolveError::ProviderUnavailable => {
            (StatusCode::SERVICE_UNAVAILABLE, "no lyrics source reachable")
        }
        ResolveError::Store(e) => {
            tracing::error!("lyrics store error: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "lyrics store error")
        }
        ResolveError::Setup(e) => {
            tracing::error!("lyrics resolver setup error: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "lyrics resolver error")
        }
    }
}

/// Wire shape. `lines` is `null` rather than `[]` on an unsynced answer so
/// a client cannot mistake "no timings" for "timings, but empty".
fn lyrics_json(row: &LyricsRow) -> Value {
    json!({
        "track_id": row.track_id.as_str(),
        "source": row.source.as_str(),
        "match_kind": row.match_kind.map(music_recommend::MatchKind::as_str),
        "synced": row.synced,
        "instrumental": row.instrumental,
        "lines": row.synced.then(|| {
            row.lines
                .iter()
                .map(|l| json!({ "start_ms": l.start_ms, "text": l.text }))
                .collect::<Vec<_>>()
        }),
        "plain": row.plain_text,
        "provider_id": row.provider_id,
        "fetched_at": row.fetched_at,
    })
}

/// Serialize with a strong ETag, answering 304 when the client already
/// has this exact payload. Worth the hash: the panel is reopened far more
/// often than lyrics change, and a full track's lines are several KB.
fn respond(row: &LyricsRow, headers: &HeaderMap) -> Response {
    let body = lyrics_json(row);
    let serialized = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
    let etag = etag_for(&serialized);

    if let Some(client_etag) = headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok())
        && client_etag.split(',').any(|candidate| candidate.trim() == etag)
    {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        apply_cache_headers(&mut response, &etag);
        return response;
    }

    let mut response = Json(body).into_response();
    apply_cache_headers(&mut response, &etag);
    response
}

fn apply_cache_headers(response: &mut Response, etag: &str) {
    if let Ok(value) = HeaderValue::from_str(etag) {
        response.headers_mut().insert(header::ETAG, value);
    }
    // `private`: the payload is catalog data, but it is served over an
    // authenticated connection and must not be held by a shared cache.
    if let Ok(value) =
        HeaderValue::from_str(&format!("private, max-age={CLIENT_MAX_AGE_SECONDS}"))
    {
        response.headers_mut().insert(header::CACHE_CONTROL, value);
    }
}

fn etag_for(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    // 16 hex chars is ample to distinguish revisions of one track's
    // lyrics; a full digest just makes headers longer.
    let mut out = String::with_capacity(18);
    out.push('"');
    for byte in digest.iter().take(8) {
        let _ = write!(out, "{byte:02x}");
    }
    out.push('"');
    out
}

/// Whether a row is worth telling the client about at all — used by the
/// diagnostics summary, not the request path.
#[must_use]
pub fn is_hit(row: &LyricsRow) -> bool {
    row.source != LyricsSource::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_recommend::{LyricLine, MatchKind};

    fn row(synced: bool) -> LyricsRow {
        LyricsRow {
            track_id: TrackId::from("tr-1".to_string()),
            source: LyricsSource::Lrclib,
            match_kind: Some(MatchKind::Exact),
            synced,
            instrumental: false,
            plain_text: Some("one".to_string()),
            lines: if synced {
                vec![LyricLine { start_ms: 1000, text: "one".to_string() }]
            } else {
                Vec::new()
            },
            provider_id: Some("42".to_string()),
            fetched_at: 7,
            expires_at: 8,
        }
    }

    #[test]
    fn unsynced_answer_has_null_lines_not_empty_lines() {
        let json = lyrics_json(&row(false));
        assert!(json["lines"].is_null());
        assert_eq!(json["plain"], "one");
        assert_eq!(json["synced"], false);
    }

    #[test]
    fn synced_answer_carries_lines() {
        let json = lyrics_json(&row(true));
        assert_eq!(json["lines"][0]["start_ms"], 1000);
        assert_eq!(json["match_kind"], "exact");
    }

    #[test]
    fn miss_serializes_as_source_none() {
        let miss = LyricsRow::miss(TrackId::from("tr-2".to_string()), 0, 10);
        let json = lyrics_json(&miss);
        assert_eq!(json["source"], "none");
        assert!(json["lines"].is_null());
        assert!(json["plain"].is_null());
        assert!(!is_hit(&miss));
    }

    #[test]
    fn etag_tracks_content() {
        let a = etag_for(b"one");
        assert_eq!(a, etag_for(b"one"));
        assert_ne!(a, etag_for(b"two"));
        assert!(a.starts_with('"') && a.ends_with('"'));
    }
}
