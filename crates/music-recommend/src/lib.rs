//! Recommender state: embedding store, ingest queue, and (later) ANN index.
//!
//! The store is content-addressed by `(track_id, model_version)`. Swapping
//! models is non-destructive: old rows remain queryable while the new
//! `model_version` slowly fills in via the background ingest worker.
//!
//! The `IngestStatus` column doubles as the queue. `not_started` rows
//! ordered by `created_at` are the work backlog; the worker transitions
//! them through `in_progress` → `done` (or `failed`).

#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

pub mod ann;
pub mod embedder;
pub mod store;
pub mod types;

pub use embedder::{EmbedResult, EmbedderClient, EmbedderConfig, EmbedderError, EmbedderHealth};
pub use store::{EmbeddingStore, MIGRATIONS};
pub use types::{Embedding, EmbeddingKey, IngestStatus, ModelVersion};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("vector dimension mismatch: stored {stored}, got {got}")]
    DimMismatch { stored: usize, got: usize },

    #[error("vector blob length {bytes} is not a multiple of 4 (corrupt f32 storage)")]
    CorruptVectorBlob { bytes: usize },

    #[error("invalid status string {0:?}")]
    InvalidStatus(String),
}

pub type Result<T> = std::result::Result<T, Error>;
