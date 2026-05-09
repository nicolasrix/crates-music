//! Background ingest worker that drains the embedding queue.
//!
//! Spawned at gateway boot when an embedder client is available. Uses a
//! `music_subsonic::Client` to range-fetch the first chunk of audio for
//! each queued track, posts to the embedder sidecar, and records the
//! result via `IngestWorker`.
//!
//! Polling pattern: when there's work, drain to empty as fast as the
//! embedder can keep up; when idle, sleep `IDLE_TICK` before polling
//! again. We don't have an in-process notification channel from the
//! enqueue endpoint, so this is straightforward polling. At single-user
//! scale (~10⁴ tracks total, dozens-per-day enqueue rate) the wasted
//! wakeups are immaterial.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use music_core::TrackId;
use music_recommend::ann::AnnIndex;
use music_recommend::embedder::EmbedderClient;
use music_recommend::ingest::{
    AudioFetcher, FetchError, IngestWorker, IngestWorkerConfig,
};
use music_recommend::store::EmbeddingStore;
use music_recommend::types::ModelVersion;
use music_subsonic::{Client as SubsonicClient, Credentials};
use reqwest::header::RANGE;
use tokio::task::JoinHandle;

use crate::config::UpstreamConfig;

/// How many bytes to range-fetch per track. CLAP's audio encoder
/// truncates / pads internally to 10 s windows, so we don't need a
/// precise duration — we just need enough bytes to decode at least one
/// window. 8 MiB at 192 kbps MP3 ≈ 5.5 minutes, plenty.
const MAX_CLIP_BYTES: u64 = 8 * 1024 * 1024;

/// Bitrate cap (kbps) we ask Navidrome to transcode to. Lossy MP3 lets
/// us truncate the byte stream mid-file without the decoder losing
/// sync — unlike FLAC, where a truncated range produces "decoder lost
/// sync" because the file ends mid-frame.
const TRANSCODE_MAX_BITRATE: u32 = 192;

/// Idle backoff between queue polls. Snappy enough for manual testing
/// while keeping per-tick cost trivial (one indexed lookup in SQLite).
const IDLE_TICK: Duration = Duration::from_secs(5);

/// Audio fetcher backed by the upstream Navidrome via the typed
/// Subsonic client. Range-caps every request at `MAX_CLIP_BYTES`.
pub struct SubsonicAudioFetcher {
    client: SubsonicClient,
}

impl std::fmt::Debug for SubsonicAudioFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubsonicAudioFetcher")
            .finish_non_exhaustive()
    }
}

impl SubsonicAudioFetcher {
    pub fn new(upstream: &UpstreamConfig) -> Result<Self> {
        let creds = Credentials {
            username: upstream.username.clone(),
            password: upstream.password.clone(),
        };
        let client = SubsonicClient::new(&upstream.navidrome_url, creds)
            .context("constructing Subsonic client for ingest")?;
        Ok(Self { client })
    }
}

#[async_trait]
impl AudioFetcher for SubsonicAudioFetcher {
    async fn fetch_clip(&self, track_id: &TrackId) -> Result<Bytes, FetchError> {
        let mut url = self
            .client
            .stream_url(track_id)
            .map_err(|e| FetchError::Transport(format!("stream_url: {e}")))?;
        // Force transcode to MP3 so the byte-range cap below produces a
        // decodable prefix. FLAC sources truncated mid-file fail with
        // "decoder lost sync" inside soundfile.
        url.query_pairs_mut()
            .append_pair("format", "mp3")
            .append_pair("maxBitRate", &TRANSCODE_MAX_BITRATE.to_string());

        let range = format!("bytes=0-{}", MAX_CLIP_BYTES - 1);
        let resp = self
            .client
            .http()
            .get(url)
            .header(RANGE, range)
            .send()
            .await
            .map_err(|e| FetchError::Transport(format!("send: {e}")))?;

        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(FetchError::NotFound(track_id.clone()));
        }
        if !status.is_success() {
            return Err(FetchError::Transport(format!("upstream {status}")));
        }
        resp.bytes()
            .await
            .map_err(|e| FetchError::Transport(format!("body: {e}")))
    }
}

/// Spawn the background ingest loop. Returns `None` (and logs) if no
/// embedder client is available — the gateway then runs in degraded
/// mode and queued rows sit at `not_started` until a future restart
/// sees the embedder.
pub fn spawn_ingest_worker(
    store: EmbeddingStore,
    ann: Arc<AnnIndex>,
    embedder: Option<EmbedderClient>,
    fetcher: Arc<dyn AudioFetcher>,
    model_version: ModelVersion,
) -> Option<JoinHandle<()>> {
    let embedder = embedder?;
    let cfg = IngestWorkerConfig {
        store,
        ann,
        embedder,
        fetcher,
        model_version: model_version.clone(),
    };
    let worker = IngestWorker::new(cfg);

    let handle = tokio::spawn(async move {
        tracing::info!(model = %model_version, "ingest: worker started");
        loop {
            match worker.drain().await {
                Ok(stats) if stats.embedded == 0 && stats.failed == 0 => {
                    tokio::time::sleep(IDLE_TICK).await;
                }
                Ok(stats) => {
                    tracing::info!(
                        embedded = stats.embedded,
                        failed = stats.failed,
                        "ingest: drained queue"
                    );
                }
                Err(e) => {
                    tracing::error!(error = %e, "ingest: drain failed; backing off");
                    tokio::time::sleep(IDLE_TICK).await;
                }
            }
        }
    });
    Some(handle)
}
