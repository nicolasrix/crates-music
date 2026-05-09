//! Single-worker ingest pipeline.
//!
//! Per track:
//!   claim_next → fetch ~120s of audio (via `AudioFetcher`)
//!              → embed via `EmbedderClient`
//!              → mark_done in store + upsert into ANN
//!
//! Failure modes:
//! - Fetch failure (transport, 5xx upstream, etc): mark_failed,
//!   IngestOutcome::Failed. Retryable via `store.reset_failed`.
//! - Embedder 503 (model_loaded=false): mark_failed with a clear
//!   error message. Retryable; once the sidecar is ready a reset
//!   re-queues these.
//! - Embedder 5xx / transport: mark_failed.
//! - SQLite write fails: surfaced as `IngestError`. Caller logs;
//!   the row stays `in_progress` and gets reset on next gateway start.
//!
//! ANN write only happens *after* SQLite mark_done succeeds — so if
//! we crash between SQLite and ANN, the SQLite row is still authoritative
//! and `rebuild_ann_from_store` recovers.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use music_core::TrackId;
use sqlx::Row;

use crate::ann::{AnnError, AnnIndex};
use crate::embedder::{EmbedderClient, EmbedderError};
use crate::store::EmbeddingStore;
use crate::types::{Embedding, EmbeddingKey, ModelVersion};

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("transport: {0}")]
    Transport(String),

    #[error("not found: {0}")]
    NotFound(TrackId),
}

#[async_trait]
pub trait AudioFetcher: Send + Sync {
    /// Fetch the first ~120 seconds of audio for the given track. The
    /// caller passes the bytes straight to the embedder, so the
    /// implementor decides how to range-request from upstream and what
    /// container format to send (CLAP-side decoding is format-agnostic
    /// — soundfile + librosa).
    async fn fetch_clip(&self, track_id: &TrackId) -> Result<Bytes, FetchError>;
}

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("store: {0}")]
    Store(#[from] crate::Error),

    #[error("ann: {0}")]
    Ann(#[from] AnnError),

    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IngestOutcome {
    /// Successfully embedded a track and updated SQLite + ANN.
    Embedded,
    /// A track was claimed but fetch or embed failed; row marked failed.
    Failed,
    /// Queue was empty — no work done.
    Idle,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrainStats {
    pub embedded: u64,
    pub failed: u64,
}

pub struct IngestWorkerConfig {
    pub store: EmbeddingStore,
    pub ann: Arc<AnnIndex>,
    pub embedder: EmbedderClient,
    pub fetcher: Arc<dyn AudioFetcher>,
    pub model_version: ModelVersion,
}

impl std::fmt::Debug for IngestWorkerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IngestWorkerConfig")
            .field("model_version", &self.model_version)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct IngestWorker {
    cfg: IngestWorkerConfig,
}

impl IngestWorker {
    pub fn new(cfg: IngestWorkerConfig) -> Self {
        Self { cfg }
    }

    /// Process one track from the queue. Returns immediately if the
    /// queue is empty. Errors here are *infrastructure* failures
    /// (SQLite, ANN); per-track failures are absorbed into
    /// `IngestOutcome::Failed`.
    ///
    /// Intentionally *not* instrumented as a span: this function is a
    /// pure wrapper around `claim_next` + `embed_one` + `mark_done` +
    /// `ann.upsert`, each already a span. Wrapping them again produces
    /// a same-duration mirror of `store.claim_next` on every idle
    /// poll, doubling trace-store growth without adding signal.
    pub async fn process_next(&self) -> Result<IngestOutcome, IngestError> {
        let Some(key) = self.cfg.store.claim_next(&self.cfg.model_version).await? else {
            return Ok(IngestOutcome::Idle);
        };

        match self.embed_one(&key).await {
            Ok(vector) => {
                self.cfg
                    .store
                    .mark_done(&Embedding::new(key.clone(), vector.clone()))
                    .await?;
                if let Err(e) = self.cfg.ann.upsert(&key.track_id, &vector) {
                    // SQLite is the source of truth; log and continue.
                    // Next startup's rebuild_ann_from_store will pick
                    // this up.
                    tracing::warn!(
                        track = %key.track_id,
                        error = %e,
                        "ANN upsert failed; SQLite row is done, ANN will catch up at restart"
                    );
                }
                Ok(IngestOutcome::Embedded)
            }
            Err(reason) => {
                tracing::warn!(track = %key.track_id, reason = %reason, "ingest failed");
                self.cfg.store.mark_failed(&key, &reason).await?;
                Ok(IngestOutcome::Failed)
            }
        }
    }

    /// Process every queued track until the queue is empty. Counts
    /// outcomes; surfaces infrastructure errors immediately (so the
    /// caller doesn't get stuck in a tight loop on a busted DB).
    pub async fn drain(&self) -> Result<DrainStats, IngestError> {
        let mut stats = DrainStats::default();
        loop {
            match self.process_next().await? {
                IngestOutcome::Embedded => stats.embedded += 1,
                IngestOutcome::Failed => stats.failed += 1,
                IngestOutcome::Idle => return Ok(stats),
            }
        }
    }

    #[tracing::instrument(name = "ingest.embed_one", skip(self), fields(track = %key.track_id))]
    async fn embed_one(&self, key: &EmbeddingKey) -> Result<Vec<f32>, String> {
        let bytes = self
            .cfg
            .fetcher
            .fetch_clip(&key.track_id)
            .await
            .map_err(|e| format!("fetch: {e}"))?;
        let result = self
            .cfg
            .embedder
            .embed_audio(bytes)
            .await
            .map_err(format_embedder_error)?;
        Ok(result.vector)
    }
}

fn format_embedder_error(e: EmbedderError) -> String {
    match e {
        EmbedderError::ModelNotLoaded => "embedder: model not loaded (503)".to_string(),
        other => format!("embedder: {other}"),
    }
}

/// Walk every `done` row for the given model_version and re-feed the
/// ANN. Used at startup when the index file is missing or stale.
pub async fn rebuild_ann_from_store(
    store: &EmbeddingStore,
    ann: &AnnIndex,
    model_version: &ModelVersion,
) -> Result<(), IngestError> {
    // Stream from SQLite to keep memory bounded; for the single-user
    // scale (≤ 10⁴ tracks × 2 KB) we could also fetch_all — but the
    // streaming form is the right shape for when this scales.
    let mut rows = sqlx::query(
        "SELECT track_id, vector FROM track_embeddings
         WHERE model_version = ? AND status = 'done' AND vector IS NOT NULL
         ORDER BY track_id",
    )
    .bind(model_version.as_str())
    .fetch(store.pool());

    let mut pairs: Vec<(TrackId, Vec<f32>)> = Vec::new();
    while let Some(row) = rows.next().await {
        let row = row?;
        let track_id: String = row.get("track_id");
        let blob: Vec<u8> = row.get("vector");
        let vector = blob_to_vector(&blob)?;
        pairs.push((TrackId::from(track_id), vector));
    }
    ann.rebuild_from(pairs.iter().map(|(t, v)| (t, v.as_slice())))?;
    Ok(())
}

fn blob_to_vector(b: &[u8]) -> Result<Vec<f32>, IngestError> {
    if !b.len().is_multiple_of(4) {
        return Err(IngestError::Store(crate::Error::CorruptVectorBlob {
            bytes: b.len(),
        }));
    }
    let mut out = Vec::with_capacity(b.len() / 4);
    for chunk in b.chunks_exact(4) {
        out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(out)
}
