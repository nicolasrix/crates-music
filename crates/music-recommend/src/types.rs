//! Domain types for the recommender.

use music_core::TrackId;
use serde::{Deserialize, Serialize};

/// Identifier for a model checkpoint. We use a free-form string rather
/// than an enum so we can include both the model name and any meaningful
/// preprocessing tweaks (e.g. `"clap-htsat-unfused-v1"`,
/// `"clap-music-audioset-fusion-v2"`). Embeddings are content-addressed
/// by `(track_id, model_version)` so different versions coexist.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelVersion(String);

impl ModelVersion {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<S: Into<String>> From<S> for ModelVersion {
    fn from(s: S) -> Self {
        Self(s.into())
    }
}

impl std::fmt::Display for ModelVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Composite key for the embedding store.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EmbeddingKey {
    pub track_id: TrackId,
    pub model_version: ModelVersion,
}

impl EmbeddingKey {
    pub fn new(track_id: impl Into<TrackId>, model_version: impl Into<ModelVersion>) -> Self {
        Self {
            track_id: track_id.into(),
            model_version: model_version.into(),
        }
    }
}

/// State of an ingest task. Doubles as the queue: `NotStarted` rows are
/// the backlog. Workers atomically transition `NotStarted` → `InProgress`
/// to claim a job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestStatus {
    NotStarted,
    InProgress,
    Done,
    Failed,
}

impl IngestStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::InProgress => "in_progress",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> crate::Result<Self> {
        match s {
            "not_started" => Ok(Self::NotStarted),
            "in_progress" => Ok(Self::InProgress),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            other => Err(crate::Error::InvalidStatus(other.to_string())),
        }
    }
}

/// A computed embedding: the float vector plus the metadata that
/// identifies which model produced it.
#[derive(Clone, Debug, PartialEq)]
pub struct Embedding {
    pub key: EmbeddingKey,
    pub vector: Vec<f32>,
}

impl Embedding {
    pub fn new(key: EmbeddingKey, vector: Vec<f32>) -> Self {
        Self { key, vector }
    }

    pub fn dim(&self) -> usize {
        self.vector.len()
    }
}
