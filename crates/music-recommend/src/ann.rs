//! Cosine-similarity ANN index over track embeddings.
//!
//! Backed by `usearch` (single-file HNSW). Two design choices that
//! load-bear:
//!
//! 1. Track ids are strings; `usearch` uses `u64` keys. We keep a
//!    parallel `(TrackId ↔ u64)` map in memory. The map is rebuildable
//!    from the SQLite store, so the index file isn't a source of truth
//!    — it's a derived cache. If it's lost, the worker re-loads from
//!    SQLite. Means we can wipe the file freely.
//! 2. Cosine *distance* in usearch is `1 - cos_sim`. We expose
//!    similarity (in `[-1, 1]`, with `1.0 == identical`) so callers
//!    don't have to remember which way the metric points.
//!
//! Single-process, lock around the index for both reads and writes.
//! Bench load is single-user with ~10⁴ tracks; mutex overhead is
//! invisible at this scale.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use music_core::TrackId;
use serde::{Deserialize, Serialize};
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

#[derive(Debug, thiserror::Error)]
pub enum AnnError {
    #[error("usearch: {0}")]
    Usearch(String),

    #[error("dimension mismatch: index expects {expected}, got {got}")]
    DimMismatch { expected: usize, got: usize },

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("lock poisoned")]
    Poisoned,
}

/// Map any usearch (cxx) error into our type. Local helper instead of
/// a `From` impl so we don't have to take `cxx` as a direct dep.
fn to_usearch_err<E: std::fmt::Display>(e: E) -> AnnError {
    AnnError::Usearch(e.to_string())
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnnQueryResult {
    pub track_id: TrackId,
    /// Cosine similarity in [-1.0, 1.0]. 1.0 = identical direction.
    pub similarity: f32,
}

pub struct AnnIndex {
    inner: RwLock<Inner>,
    dim: usize,
}

impl std::fmt::Debug for AnnIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnnIndex")
            .field("dim", &self.dim)
            .finish_non_exhaustive()
    }
}

struct Inner {
    index: Index,
    /// Bidirectional map between domain ids and the u64 keys usearch wants.
    forward: HashMap<TrackId, u64>,
    reverse: HashMap<u64, TrackId>,
    /// Monotonic counter for fresh u64 keys. We never reuse keys —
    /// once a track is removed, its slot is dead.
    next_key: u64,
    /// Set when `open(path, ...)` was used; `persist()` writes here.
    persist_path: Option<PathBuf>,
}

impl AnnIndex {
    /// Open a fresh in-memory index. Useful for tests and for the
    /// rebuild-from-sqlite path.
    pub fn open_in_memory(dim: usize, connectivity: usize) -> Result<Self, AnnError> {
        let index = build_index(dim, connectivity)?;
        Ok(Self {
            inner: RwLock::new(Inner {
                index,
                forward: HashMap::new(),
                reverse: HashMap::new(),
                next_key: 1,
                persist_path: None,
            }),
            dim,
        })
    }

    /// Open or create an index at `path`. If the file exists, load
    /// it; otherwise the index is empty and only the path is
    /// remembered for future `persist()` calls.
    ///
    /// usearch persists the HNSW graph + vectors but NOT our
    /// `(TrackId ↔ u64)` map, so we write a sidecar JSON file
    /// (`<path>.keys`) and load it back on open. If the keys file is
    /// missing or stale, callers can recover via `rebuild_from`
    /// (the worker does this at startup against SQLite).
    pub fn open(path: &Path, dim: usize, connectivity: usize) -> Result<Self, AnnError> {
        let index = build_index(dim, connectivity)?;
        let keys_path = sidecar_keys_path(path);
        let mut forward = HashMap::new();
        let mut reverse = HashMap::new();
        let mut next_key = 1u64;
        if path.exists() {
            let s = path.to_string_lossy();
            index.load(&s).map_err(to_usearch_err)?;
        }
        if keys_path.exists() {
            let raw = std::fs::read_to_string(&keys_path)?;
            let snapshot: KeysSnapshot = serde_json::from_str(&raw)
                .map_err(|e| AnnError::Usearch(format!("bad keys sidecar: {e}")))?;
            next_key = snapshot.next_key.max(1);
            for entry in snapshot.entries {
                let id = TrackId::from(entry.track_id);
                forward.insert(id.clone(), entry.key);
                reverse.insert(entry.key, id);
            }
        }
        Ok(Self {
            inner: RwLock::new(Inner {
                index,
                forward,
                reverse,
                next_key,
                persist_path: Some(path.to_path_buf()),
            }),
            dim,
        })
    }

    pub fn len(&self) -> Result<usize, AnnError> {
        let inner = self.inner.read().map_err(|_| AnnError::Poisoned)?;
        Ok(inner.forward.len())
    }

    pub fn is_empty(&self) -> Result<bool, AnnError> {
        Ok(self.len()? == 0)
    }

    #[tracing::instrument(name = "ann.upsert", skip(self, vector), fields(track = %track_id))]
    pub fn upsert(&self, track_id: &TrackId, vector: &[f32]) -> Result<(), AnnError> {
        if vector.len() != self.dim {
            return Err(AnnError::DimMismatch {
                expected: self.dim,
                got: vector.len(),
            });
        }
        let mut inner = self.inner.write().map_err(|_| AnnError::Poisoned)?;
        // Replace path: usearch supports duplicate-key removal via
        // `remove`, so we evict the old vector before inserting the
        // new one. Cheaper than supporting `multi=true` and filtering.
        if let Some(&existing_key) = inner.forward.get(track_id) {
            inner.index.remove(existing_key).map_err(to_usearch_err)?;
            inner.reverse.remove(&existing_key);
        }
        let key = inner.next_key;
        inner.next_key += 1;
        // Reserve grows the index lazily but we cap-double when we hit
        // capacity to avoid re-reserving on every add.
        ensure_capacity(&inner.index, inner.forward.len() + 1)?;
        inner.index.add(key, vector).map_err(to_usearch_err)?;
        inner.forward.insert(track_id.clone(), key);
        inner.reverse.insert(key, track_id.clone());
        Ok(())
    }

    pub fn remove(&self, track_id: &TrackId) -> Result<(), AnnError> {
        let mut inner = self.inner.write().map_err(|_| AnnError::Poisoned)?;
        if let Some(key) = inner.forward.remove(track_id) {
            inner.reverse.remove(&key);
            inner.index.remove(key).map_err(to_usearch_err)?;
        }
        Ok(())
    }

    pub fn query(&self, query: &[f32], k: usize) -> Result<Vec<AnnQueryResult>, AnnError> {
        self.query_excluding(query, k, &[])
    }

    /// Retrieve the stored vector for a track id, if present. Useful
    /// when the caller wants "find similar to X" but only has X's id —
    /// the ANN already has the vector, so we don't need a SQLite hop.
    pub fn get_vector(&self, track_id: &TrackId) -> Result<Option<Vec<f32>>, AnnError> {
        let inner = self.inner.read().map_err(|_| AnnError::Poisoned)?;
        let Some(&key) = inner.forward.get(track_id) else {
            return Ok(None);
        };
        let mut buf = vec![0.0_f32; self.dim];
        let n = inner.index.get(key, &mut buf).map_err(to_usearch_err)?;
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(buf))
    }

    /// Query with an exclusion list. Common pattern: "give me 5 tracks
    /// similar to seed X, but not X itself or anything I've already
    /// queued." We over-fetch a bit and filter in-process.
    pub fn query_excluding(
        &self,
        query: &[f32],
        k: usize,
        exclude: &[TrackId],
    ) -> Result<Vec<AnnQueryResult>, AnnError> {
        if query.len() != self.dim {
            return Err(AnnError::DimMismatch {
                expected: self.dim,
                got: query.len(),
            });
        }
        if k == 0 {
            return Ok(Vec::new());
        }
        let inner = self.inner.read().map_err(|_| AnnError::Poisoned)?;
        // Over-fetch by `exclude.len()` so we can drop matches and
        // still hit `k`. usearch caps fetches at the index size.
        let want = (k + exclude.len()).min(inner.forward.len());
        if want == 0 {
            return Ok(Vec::new());
        }
        let matches = inner.index.search(query, want).map_err(to_usearch_err)?;
        let mut out = Vec::with_capacity(k);
        for (key, distance) in matches.keys.iter().zip(matches.distances.iter()) {
            let Some(track_id) = inner.reverse.get(key).cloned() else {
                continue;
            };
            if exclude.contains(&track_id) {
                continue;
            }
            out.push(AnnQueryResult {
                track_id,
                similarity: 1.0 - *distance,
            });
            if out.len() == k {
                break;
            }
        }
        Ok(out)
    }

    /// Persist the index + key sidecar to the configured path. No-op
    /// for in-memory indices.
    pub fn persist(&self) -> Result<(), AnnError> {
        let inner = self.inner.read().map_err(|_| AnnError::Poisoned)?;
        let Some(path) = &inner.persist_path else {
            return Ok(());
        };
        let s = path.to_string_lossy();
        inner.index.save(&s).map_err(to_usearch_err)?;

        let snapshot = KeysSnapshot {
            next_key: inner.next_key,
            entries: inner
                .forward
                .iter()
                .map(|(t, k)| KeysEntry {
                    key: *k,
                    track_id: t.as_str().to_string(),
                })
                .collect(),
        };
        let keys_path = sidecar_keys_path(path);
        let json = serde_json::to_string(&snapshot)
            .map_err(|e| AnnError::Usearch(format!("serialize keys: {e}")))?;
        std::fs::write(&keys_path, json)?;
        Ok(())
    }

    /// Bulk-load (track_id, vector) pairs into an empty (or about-to-be-cleared)
    /// index. Used by the recovery path: at startup, the worker walks
    /// SQLite for every `(track_id, model_version)` with `status = done`
    /// and feeds them here.
    pub fn rebuild_from<'a, I>(&self, pairs: I) -> Result<(), AnnError>
    where
        I: IntoIterator<Item = (&'a TrackId, &'a [f32])>,
    {
        let mut inner = self.inner.write().map_err(|_| AnnError::Poisoned)?;
        inner.index.reset().map_err(to_usearch_err)?;
        inner.forward.clear();
        inner.reverse.clear();
        inner.next_key = 1;
        for (id, v) in pairs {
            if v.len() != self.dim {
                return Err(AnnError::DimMismatch {
                    expected: self.dim,
                    got: v.len(),
                });
            }
            let key = inner.next_key;
            inner.next_key += 1;
            ensure_capacity(&inner.index, inner.forward.len() + 1)?;
            inner.index.add(key, v).map_err(to_usearch_err)?;
            inner.forward.insert(id.clone(), key);
            inner.reverse.insert(key, id.clone());
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct KeysSnapshot {
    next_key: u64,
    entries: Vec<KeysEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct KeysEntry {
    key: u64,
    track_id: String,
}

fn sidecar_keys_path(index_path: &Path) -> PathBuf {
    let mut p = index_path.as_os_str().to_owned();
    p.push(".keys");
    PathBuf::from(p)
}

fn build_index(dim: usize, connectivity: usize) -> Result<Index, AnnError> {
    let opts = IndexOptions {
        dimensions: dim,
        metric: MetricKind::Cos,
        quantization: ScalarKind::F32,
        connectivity,
        // Defaults from usearch — leave the search-time and add-time
        // expansion at 0 so the C++ side picks sensible values
        // (typically 64 / 16). Tunable later from gateway config.
        expansion_add: 0,
        expansion_search: 0,
        multi: false,
    };
    Index::new(&opts).map_err(to_usearch_err)
}

fn ensure_capacity(index: &Index, needed: usize) -> Result<(), AnnError> {
    // usearch grows lazily; calling reserve keeps add() O(1) amortised.
    // We round up to the next power of two so reserves are infrequent.
    let cap = needed.next_power_of_two().max(64);
    index.reserve(cap).map_err(to_usearch_err)?;
    Ok(())
}
