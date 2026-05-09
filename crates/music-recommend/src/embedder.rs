//! HTTP client for the Python embedder sidecar (`services/embedder/`).
//!
//! Three operations:
//! - `healthz`: reachability + model_loaded probe. Returns a parsed
//!   `EmbedderHealth` for any 2xx **or 503** — 503 carries the
//!   "loading" signal in its body and is the sidecar's well-defined
//!   way of saying "I'm alive but not ready." Genuinely-unreachable
//!   gives `Transport`.
//! - `embed_audio`: raw bytes → vector. Bytes go straight onto the
//!   wire as `application/octet-stream` (matching the FastAPI side).
//! - `embed_text`: JSON `{text}` → vector.
//!
//! `503 model not loaded` from `embed_*` maps to a distinct
//! `ModelNotLoaded` error variant so callers can decide:
//! "retry later, sidecar is alive" (503) vs "this is broken" (5xx).

use std::time::Duration;

use bytes::Bytes;
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use tracing::field;
use url::Url;

use crate::types::ModelVersion;

/// Parse a `Server-Timing` HTTP header into `(metric, dur_ms)` pairs.
///
/// Per RFC, an entry is `metric;param=value, metric2;param=value`. We
/// pluck only `dur=` because that's the only number we care about;
/// other params (`desc=`) are ignored. Entries with no `dur=` and
/// entries with unparseable values are dropped — this is parsing
/// telemetry, not a wire contract, so be lenient.
///
/// Pure function, exhaustively unit-tested in
/// `tests/embedder_client.rs::server_timing_parser`.
#[must_use]
pub fn parse_server_timing(header: &str) -> Vec<(String, f64)> {
    header
        .split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            let mut parts = entry.split(';');
            let name = parts.next()?.trim();
            if name.is_empty() {
                return None;
            }
            let mut dur: Option<f64> = None;
            for param in parts {
                let param = param.trim();
                if let Some(rest) = param.strip_prefix("dur=") {
                    dur = rest.trim().parse().ok();
                }
            }
            dur.map(|d| (name.to_string(), d))
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct EmbedderConfig {
    pub url: Url,
    pub timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct EmbedderClient {
    http: Client,
    base: Url,
}

#[derive(Debug, thiserror::Error)]
pub enum EmbedderError {
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("model not loaded (503)")]
    ModelNotLoaded,

    #[error("server error {status}: {body}")]
    Server { status: u16, body: String },

    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct EmbedderHealth {
    /// True if the GET /healthz returned any HTTP status (i.e., the
    /// network and the FastAPI app are alive).
    pub reachable: bool,
    pub model_loaded: bool,
    pub model_version: ModelVersion,
    pub dim: usize,
    /// Compute device the sidecar is running on, e.g. "cpu" or "cuda".
    /// `None` when the sidecar pre-dates the device field — gateway
    /// surfaces this as "device=unknown" in its boot log rather than
    /// failing the probe.
    pub device: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EmbedResult {
    pub vector: Vec<f32>,
    pub dim: usize,
    pub model_version: ModelVersion,
}

impl EmbedderClient {
    pub fn new(config: EmbedderConfig) -> Result<Self, EmbedderError> {
        let http = Client::builder().timeout(config.timeout).build()?;
        Ok(Self {
            http,
            base: config.url,
        })
    }

    pub async fn healthz(&self) -> Result<EmbedderHealth, EmbedderError> {
        let url = join(&self.base, "/healthz");
        let resp = self.http.get(url).send().await?;
        let status = resp.status();
        // Accept 2xx and 503: both carry the JSON envelope.
        if status.is_success() || status == StatusCode::SERVICE_UNAVAILABLE {
            let body: HealthBody = parse_json(resp).await?;
            return Ok(EmbedderHealth {
                reachable: true,
                model_loaded: body.model_loaded,
                model_version: ModelVersion::from(body.model_version),
                dim: body.dim,
                device: body.device,
            });
        }
        Err(server_error(resp).await)
    }

    #[tracing::instrument(
        name = "embedder.embed_audio",
        skip(self, audio),
        fields(bytes = audio.len(), server_timing = field::Empty)
    )]
    pub async fn embed_audio(&self, audio: Bytes) -> Result<EmbedResult, EmbedderError> {
        let url = join(&self.base, "/embed/audio");
        let resp = self
            .http
            .post(url)
            .header("content-type", "application/octet-stream")
            .body(audio)
            .send()
            .await?;
        Self::parse_embed_response(resp).await
    }

    #[tracing::instrument(name = "embedder.embed_text", skip(self), fields(server_timing = field::Empty))]
    pub async fn embed_text(&self, text: &str) -> Result<EmbedResult, EmbedderError> {
        let url = join(&self.base, "/embed/text");
        let resp = self
            .http
            .post(url)
            .json(&serde_json::json!({ "text": text }))
            .send()
            .await?;
        Self::parse_embed_response(resp).await
    }

    async fn parse_embed_response(resp: reqwest::Response) -> Result<EmbedResult, EmbedderError> {
        let status = resp.status();
        if status == StatusCode::SERVICE_UNAVAILABLE {
            return Err(EmbedderError::ModelNotLoaded);
        }
        if !status.is_success() {
            return Err(server_error(resp).await);
        }
        // Pluck Server-Timing before consuming the body — `resp.text()`
        // moves the response, dropping the headers. `record` is a
        // no-op when the field isn't declared on the current span, so
        // this is safe to call regardless of caller context.
        if let Some(timing) = resp
            .headers()
            .get("server-timing")
            .and_then(|v| v.to_str().ok())
        {
            tracing::Span::current().record("server_timing", timing);
        }
        let body: EmbedBody = parse_json(resp).await?;
        if body.dim == 0 || body.vector.is_empty() {
            return Err(EmbedderError::InvalidResponse(format!(
                "embedding has zero dimensions (dim={}, vector_len={})",
                body.dim,
                body.vector.len()
            )));
        }
        if body.vector.len() != body.dim {
            return Err(EmbedderError::InvalidResponse(format!(
                "dim mismatch: announced {}, got {}",
                body.dim,
                body.vector.len()
            )));
        }
        Ok(EmbedResult {
            vector: body.vector,
            dim: body.dim,
            model_version: ModelVersion::from(body.model_version),
        })
    }
}

#[derive(Deserialize)]
struct HealthBody {
    model_loaded: bool,
    model_version: String,
    dim: usize,
    /// Optional: older sidecars don't emit this. `serde(default)` parses
    /// the missing-field case as `None` instead of failing.
    #[serde(default)]
    device: Option<String>,
}

#[derive(Deserialize)]
struct EmbedBody {
    vector: Vec<f32>,
    dim: usize,
    model_version: String,
}

fn join(base: &Url, path: &str) -> Url {
    let mut url = base.clone();
    url.set_path(path);
    url
}

async fn parse_json<T: for<'de> Deserialize<'de>>(
    resp: reqwest::Response,
) -> Result<T, EmbedderError> {
    let body = resp.text().await?;
    serde_json::from_str::<T>(&body)
        .map_err(|e| EmbedderError::InvalidResponse(format!("parse: {e}; body={body}")))
}

async fn server_error(resp: reqwest::Response) -> EmbedderError {
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    EmbedderError::Server { status, body }
}
