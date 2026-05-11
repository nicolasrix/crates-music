//! Dedicated `/rest/scrobble` handler.
//!
//! Subsonic's scrobble endpoint normally falls through the catch-all
//! proxy. We intercept it for two reasons:
//!
//!   * The recommender needs a fast-path "when was this track last
//!     played" lookup for the MMR recency penalty, and scanning the
//!     event log on every recommend hot-path lookup would be too slow.
//!     → write to `play_history` (track_id PK, last_played_ms).
//!   * The diagnostics panel + future behavioural index need a durable
//!     append-only signal of every play.
//!     → append a `Scrobble` event to the event log.
//!
//! Both writes happen on submission scrobbles only — now-playing pings
//! (`submission=false`) are hint-only; the user may abandon the track.
//!
//! The handler:
//!   1. Parses query params (`id`, `submission`, `time`).
//!   2. On submission: writes `play_history` and appends to the event
//!      log. Both writes are best-effort: we log on failure and never
//!      block the forwarded request, since Navidrome remains the
//!      canonical play-count ledger.
//!   3. Forwards the unmodified request to the upstream proxy.
//!
//! `time` is the client-supplied unix-ms timestamp of the play. We
//! prefer it over server-side `now` because offline batches may
//! deliver scrobbles minutes or hours after the play actually
//! happened, and the event log's `occurred_at` should reflect the
//! user's clock, not the gateway's.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Query, Request, State};
use axum::response::Response;
use music_core::TrackId;
use music_recommend::{EventInput, EventType};
use serde::Deserialize;

use crate::proxy::proxy;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ScrobbleQuery {
    pub id: Option<String>,
    /// Optional in the Subsonic spec — omitted means `true`. We accept
    /// both `"true"`/`"false"` and `"1"`/`"0"` to match Navidrome's
    /// liberal parsing.
    pub submission: Option<String>,
    /// Subsonic `time` — client-supplied unix-ms timestamp. Optional;
    /// if absent we fall back to the gateway's wall clock.
    pub time: Option<i64>,
}

fn parse_submission(raw: Option<&str>) -> bool {
    !matches!(raw, Some("false" | "0"))
}

fn now_ms() -> i64 {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(dur.as_millis()).unwrap_or(i64::MAX)
}

#[tracing::instrument(name = "scrobble.handler", skip_all, fields(track_id = tracing::field::Empty, submission = tracing::field::Empty))]
pub async fn scrobble(
    State(state): State<AppState>,
    Query(q): Query<ScrobbleQuery>,
    request: Request,
) -> Response {
    let is_submission = parse_submission(q.submission.as_deref());
    tracing::Span::current().record("submission", is_submission);

    if is_submission && let Some(id) = q.id.as_deref() {
        tracing::Span::current().record("track_id", id);
        let track_id = TrackId::from(id.to_string());
        let occurred_at = q.time.unwrap_or_else(now_ms);

        // Fast-path recency clock for the MMR penalty.
        if let Err(err) = state
            .play_history()
            .record_submission(&track_id, occurred_at)
            .await
        {
            tracing::warn!(error = %err, "play_history write failed; continuing with forward");
        }

        // Durable signal for diagnostics + future behavioural index.
        let event = EventInput {
            event_type: EventType::Scrobble,
            track_id,
            occurred_at,
            metadata: None,
        };
        if let Err(err) = state.event_store().append_batch(&[event]).await {
            tracing::warn!(error = %err, "event log append failed; continuing with forward");
        }
    }

    proxy(State(state), request).await
}

#[cfg(test)]
mod tests {
    use super::parse_submission;

    #[test]
    fn submission_defaults_to_true_when_absent() {
        assert!(parse_submission(None));
    }

    #[test]
    fn submission_true_string_is_true() {
        assert!(parse_submission(Some("true")));
    }

    #[test]
    fn submission_false_string_is_false() {
        assert!(!parse_submission(Some("false")));
    }

    #[test]
    fn submission_legacy_zero_is_false() {
        assert!(!parse_submission(Some("0")));
    }

    #[test]
    fn submission_legacy_one_is_true() {
        // Anything that isn't "false" or "0" gets the default-true
        // treatment. Matches Navidrome's liberal parsing.
        assert!(parse_submission(Some("1")));
    }

    #[test]
    fn submission_unknown_is_true() {
        // Garbage in → default to true (i.e., treat as a real scrobble).
        // Subsonic clients that omit the param entirely also land here,
        // and we want those to count.
        assert!(parse_submission(Some("yes")));
    }
}
