//! `/readyz` — strict readiness probe.
//!
//! Liveness (`/healthz`) is unconditional: as long as the axum runtime
//! is up, the answer is 200. Readiness is conditional: it asks "should
//! traffic land on this container *right now?*" — which means actually
//! probing the dependencies the gateway opted into.
//!
//! Dependencies:
//! - **Navidrome upstream** — required. Without it, nothing under
//!   `/rest/*` works and most of `/v1/*` falls back to stale cache.
//!   Probed every call via Subsonic `ping`, with a 2 s overall timeout.
//! - **Embedder sidecar** — required only when `[embedder]` is in
//!   config. Read from the in-process `EmbedderHandle` (the boot probe
//!   recorded it; ingest workers refresh it). No network call here.
//!
//! The Docker `HEALTHCHECK` points at this endpoint, so a flap in
//! Navidrome flips the container to `unhealthy` within one healthcheck
//! interval (~30 s by default) — that's what makes
//! `depends_on: service_healthy` meaningful in compose.

use std::time::Duration;

use axum::{Json, extract::State, http::StatusCode};
use music_subsonic::{Client as SubsonicClient, Credentials};
use serde_json::{Value, json};
use tokio::time::timeout;

use crate::state::AppState;

/// Overall budget for the upstream Navidrome probe. Kept short so a
/// healthcheck never serialises behind a hanging upstream — the goal
/// is "tell me fast whether it's reachable", not "wait for it".
const NAVIDROME_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

pub async fn readyz(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let navidrome = probe_navidrome(&state).await;
    let embedder = probe_embedder(&state);

    let all_ok = is_ok(&navidrome) && is_ok(&embedder);
    let status_code = if all_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let overall = if all_ok { "ready" } else { "degraded" };

    (
        status_code,
        Json(json!({
            "status": overall,
            "service": "music-gateway",
            "checks": {
                "navidrome": navidrome,
                "embedder": embedder,
            }
        })),
    )
}

fn is_ok(check: &Value) -> bool {
    matches!(
        check.get("status").and_then(Value::as_str),
        Some("ok" | "disabled")
    )
}

async fn probe_navidrome(state: &AppState) -> Value {
    let upstream = &state.config().upstream;
    let creds = Credentials {
        username: upstream.username.clone(),
        password: upstream.password.clone(),
    };
    let client = match SubsonicClient::new(&upstream.navidrome_url, creds) {
        Ok(c) => c,
        Err(e) => return error(format!("invalid upstream url: {e}")),
    };
    match timeout(NAVIDROME_PROBE_TIMEOUT, client.ping()).await {
        Ok(Ok(())) => json!({ "status": "ok" }),
        Ok(Err(e)) => error(format!("{e}")),
        Err(_) => error(format!(
            "timeout after {}s",
            NAVIDROME_PROBE_TIMEOUT.as_secs()
        )),
    }
}

fn probe_embedder(state: &AppState) -> Value {
    let handle = state.embedder();
    if handle.client().is_none() {
        // No [embedder] block in config — explicit opt-out. The
        // recommender runs in tag-only degraded mode but the gateway
        // itself is still ready.
        return json!({ "status": "disabled" });
    }
    if handle.ready() {
        let dim = handle.last_health().map(|h| h.dim);
        json!({
            "status": "ok",
            "dim": dim,
        })
    } else {
        let detail = handle
            .last_health()
            .map(|h| {
                if h.reachable {
                    format!("model not loaded (version={})", h.model_version)
                } else {
                    "unreachable".to_string()
                }
            })
            .unwrap_or_else(|| "never reached at boot".to_string());
        error(detail)
    }
}

fn error(msg: impl Into<String>) -> Value {
    json!({ "status": "error", "error": msg.into() })
}
