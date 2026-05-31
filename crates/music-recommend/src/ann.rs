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
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use music_core::TrackId;
use serde::{Deserialize, Serialize};
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use crate::whitening::Whitening;

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

    #[error("whitening: {0}")]
    Whitening(String),
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
    /// Set by every mutating call (`upsert`, `remove`, `rebuild_from`),
    /// cleared by `persist`. Lets a background task call
    /// `persist_if_dirty` on a tick without taking the inner write
    /// lock unless there's actually something to flush. Atomic so the
    /// signal is lock-free.
    dirty: AtomicBool,
    /// Optional All-but-the-Top whitening transform. When `Some`, every
    /// raw vector that *enters* the index — via `upsert`, `rebuild_from`,
    /// or as a `query` input — is whitened first, so the HNSW holds (and
    /// compares) de-coned vectors. `None` is identity: the index behaves
    /// exactly as it did before whitening existed (this is what tests and
    /// the `whitening_enabled = false` config path rely on).
    ///
    /// Held in its own lock so a refit (`set_whitening`) doesn't contend
    /// with the index lock, and so a `query` can clone the `Arc` out
    /// without holding a lock across the search. `get_vector` returns the
    /// *stored* (already-whitened) vector — that's correct for MMR's
    /// candidate-vs-candidate cosine and is why no caller re-whitens.
    whitening: RwLock<Option<Arc<Whitening>>>,
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
            dirty: AtomicBool::new(false),
            whitening: RwLock::new(None),
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
            // A freshly-opened index reflects whatever's on disk. Not
            // dirty until something mutates it.
            dirty: AtomicBool::new(false),
            whitening: RwLock::new(None),
        })
    }

    /// Install (or clear) the whitening transform. Subsequent `upsert`,
    /// `rebuild_from`, and `query` calls apply it. Changing the transform
    /// does NOT retroactively re-whiten vectors already in the index — the
    /// caller must follow a `set_whitening` with a `rebuild_from` so the
    /// stored vectors match the new transform. Rejects a transform whose
    /// dimensionality doesn't match the index.
    pub fn set_whitening(&self, whitening: Option<Arc<Whitening>>) -> Result<(), AnnError> {
        if let Some(w) = &whitening
            && w.dim() != self.dim
        {
            return Err(AnnError::DimMismatch {
                expected: self.dim,
                got: w.dim(),
            });
        }
        let mut guard = self.whitening.write().map_err(|_| AnnError::Poisoned)?;
        *guard = whitening;
        Ok(())
    }

    /// Whether a whitening transform is currently installed.
    pub fn has_whitening(&self) -> bool {
        self.whitening.read().is_ok_and(|g| g.is_some())
    }

    /// Apply the installed transform to a raw vector, or pass it through
    /// unchanged when whitening is disabled. Returns an owned vector
    /// either way — the per-call clone (≈3 KB at dim 768) is negligible
    /// next to the HNSW search it precedes.
    fn whiten(&self, raw: &[f32]) -> Result<Vec<f32>, AnnError> {
        let guard = self.whitening.read().map_err(|_| AnnError::Poisoned)?;
        match guard.as_ref() {
            Some(w) => w
                .transform(raw)
                .map_err(|e| AnnError::Whitening(e.to_string())),
            None => Ok(raw.to_vec()),
        }
    }

    /// Whiten a raw *text* query: centers by the cross-modal text mean
    /// (when fitted) so a station query lands in the same whitened space
    /// as the stored audio vectors. Identity when whitening is disabled.
    fn whiten_text(&self, raw: &[f32]) -> Result<Vec<f32>, AnnError> {
        let guard = self.whitening.read().map_err(|_| AnnError::Poisoned)?;
        match guard.as_ref() {
            Some(w) => w
                .transform_text(raw)
                .map_err(|e| AnnError::Whitening(e.to_string())),
            None => Ok(raw.to_vec()),
        }
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
        // Whiten on the way in (identity when disabled). Dimensionality
        // is preserved, so the stored vector is still `self.dim`-long.
        let vector = self.whiten(vector)?;
        let vector = vector.as_slice();
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
        drop(inner);
        self.dirty.store(true, Ordering::Release);
        Ok(())
    }

    pub fn remove(&self, track_id: &TrackId) -> Result<(), AnnError> {
        let mut inner = self.inner.write().map_err(|_| AnnError::Poisoned)?;
        let removed = if let Some(key) = inner.forward.remove(track_id) {
            inner.reverse.remove(&key);
            inner.index.remove(key).map_err(to_usearch_err)?;
            true
        } else {
            false
        };
        drop(inner);
        if removed {
            self.dirty.store(true, Ordering::Release);
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
    ///
    /// Span fields:
    /// - `k`, `excluded`, `returned` — request shape and result count.
    /// - `index_size` — number of vectors currently in the HNSW.
    /// - `search_ns` — wall-clock ns spent inside `usearch::search`
    ///   only (excludes validation, lock acquisition, exclusion
    ///   filtering). This is the cost that scales with catalog size.
    /// - `ns_per_vector` — `search_ns / index_size`. Catalog-size
    ///   normalised perf indicator. HNSW is sub-linear, so this
    ///   trends *down* as the index grows; it is not constant. Useful
    ///   for catching "search got 5× slower per vector" regressions
    ///   without chasing absolute-time noise as the catalog evolves.
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
        // Whiten the query into the same space as the stored vectors
        // (identity when disabled). Raw seed vectors arrive here; a seed
        // already in the index is fetched + whitened upstream by its raw
        // store row, never via `get_vector`, so it's whitened exactly once.
        let query = self.whiten(query)?;
        self.search_whitened(&query, k, exclude)
    }

    /// Text-query search (stations). Whitens via the cross-modal text mean
    /// so a raw text embedding lands in the same whitened space as the
    /// stored audio vectors — without this, audio-fit whitening collapses
    /// text queries together. No exclusion list: stations seed from text,
    /// not a track id.
    pub fn query_text(&self, query: &[f32], k: usize) -> Result<Vec<AnnQueryResult>, AnnError> {
        if query.len() != self.dim {
            return Err(AnnError::DimMismatch {
                expected: self.dim,
                got: query.len(),
            });
        }
        if k == 0 {
            return Ok(Vec::new());
        }
        let query = self.whiten_text(query)?;
        self.search_whitened(&query, k, &[])
    }

    /// Core HNSW search over an already-whitened query vector. Shared by
    /// the audio (`query`/`query_excluding`) and text (`query_text`) paths.
    ///
    /// Span fields:
    /// - `k`, `excluded`, `returned` — request shape and result count.
    /// - `index_size` — number of vectors currently in the HNSW.
    /// - `search_ns` — wall-clock ns spent inside `usearch::search`
    ///   only (excludes validation, lock acquisition, exclusion
    ///   filtering). This is the cost that scales with catalog size.
    /// - `ns_per_vector` — `search_ns / index_size`. Catalog-size
    ///   normalised perf indicator. HNSW is sub-linear, so this
    ///   trends *down* as the index grows; it is not constant.
    #[tracing::instrument(
        name = "ann.query",
        skip_all,
        fields(
            k = k,
            excluded = exclude.len(),
            returned = tracing::field::Empty,
            index_size = tracing::field::Empty,
            search_ns = tracing::field::Empty,
            ns_per_vector = tracing::field::Empty,
        ),
    )]
    fn search_whitened(
        &self,
        query: &[f32],
        k: usize,
        exclude: &[TrackId],
    ) -> Result<Vec<AnnQueryResult>, AnnError> {
        let inner = self.inner.read().map_err(|_| AnnError::Poisoned)?;
        let index_size = inner.forward.len();
        // Over-fetch by `exclude.len()` so we can drop matches and
        // still hit `k`. usearch caps fetches at the index size.
        let want = (k + exclude.len()).min(index_size);
        let span = tracing::Span::current();
        span.record("index_size", index_size);
        if want == 0 {
            return Ok(Vec::new());
        }
        let t_search = std::time::Instant::now();
        let matches = inner.index.search(query, want).map_err(to_usearch_err)?;
        let search_ns = u64::try_from(t_search.elapsed().as_nanos()).unwrap_or(u64::MAX);
        span.record("search_ns", search_ns);
        if index_size > 0 {
            span.record("ns_per_vector", search_ns / index_size as u64);
        }
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
        span.record("returned", out.len());
        Ok(out)
    }

    /// Persist the index + key sidecar to the configured path. No-op
    /// for in-memory indices. Always clears the dirty flag on success
    /// — even if the index was opened in-memory and the disk write
    /// was a no-op, "dirty" semantically means "in-memory state has
    /// drifted from on-disk state," and the in-memory case has no
    /// drift to track.
    pub fn persist(&self) -> Result<(), AnnError> {
        let inner = self.inner.read().map_err(|_| AnnError::Poisoned)?;
        let Some(path) = &inner.persist_path else {
            self.dirty.store(false, Ordering::Release);
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
        self.dirty.store(false, Ordering::Release);
        Ok(())
    }

    /// Atomically clear the dirty flag and persist if it was set.
    /// Returns `Ok(true)` if a write happened, `Ok(false)` if the
    /// index was already clean. Designed for periodic background
    /// persistence — racing with concurrent upserts is benign:
    ///
    ///  - upsert-then-swap: we persist with the upsert included.
    ///  - swap-then-upsert: dirty is set again, next tick catches it.
    ///
    /// On persist failure the dirty flag is restored so a future tick
    /// retries.
    pub fn persist_if_dirty(&self) -> Result<bool, AnnError> {
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return Ok(false);
        }
        match self.persist() {
            Ok(()) => Ok(true),
            Err(e) => {
                // persist() already cleared dirty on the success path;
                // on failure we re-arm so the next tick retries.
                self.dirty.store(true, Ordering::Release);
                Err(e)
            }
        }
    }

    /// Bulk-load (track_id, vector) pairs into an empty (or about-to-be-cleared)
    /// index. Used by the recovery path: at startup, the worker walks
    /// SQLite for every `(track_id, model_version)` with `status = done`
    /// and feeds them here.
    pub fn rebuild_from<'a, I>(&self, pairs: I) -> Result<(), AnnError>
    where
        I: IntoIterator<Item = (&'a TrackId, &'a [f32])>,
    {
        // Whiten (and dim-check) every vector *before* taking the index write
        // lock. The transform is pure CPU work; holding the write lock across
        // the whole corpus (≤10⁴ vectors) would stall every concurrent query
        // and upsert for the duration of the rebuild. Snapshot the transform
        // once (clone the Arc, drop the guard) so the prep loop is lock-free.
        let whitening = self.whitening.read().map_err(|_| AnnError::Poisoned)?.clone();
        let prepared: Vec<(TrackId, Vec<f32>)> = pairs
            .into_iter()
            .map(|(id, v)| {
                if v.len() != self.dim {
                    return Err(AnnError::DimMismatch {
                        expected: self.dim,
                        got: v.len(),
                    });
                }
                let whitened = match &whitening {
                    Some(w) => w
                        .transform(v)
                        .map_err(|e| AnnError::Whitening(e.to_string()))?,
                    None => v.to_vec(),
                };
                Ok((id.clone(), whitened))
            })
            .collect::<Result<_, AnnError>>()?;

        // Lock held only for the index mutations now.
        let mut inner = self.inner.write().map_err(|_| AnnError::Poisoned)?;
        inner.index.reset().map_err(to_usearch_err)?;
        inner.forward.clear();
        inner.reverse.clear();
        inner.next_key = 1;
        for (id, whitened) in prepared {
            let key = inner.next_key;
            inner.next_key += 1;
            ensure_capacity(&inner.index, inner.forward.len() + 1)?;
            inner.index.add(key, &whitened).map_err(to_usearch_err)?;
            inner.forward.insert(id.clone(), key);
            inner.reverse.insert(key, id);
        }
        drop(inner);
        self.dirty.store(true, Ordering::Release);
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

#[cfg(test)]
mod tests {
    // Test fixtures build vectors from small integer indices; the f32
    // casts are exact at these magnitudes.
    #![allow(clippy::cast_precision_loss)]
    use super::*;
    use crate::whitening::Whitening;

    /// Strongly anisotropic corpus: a large shared component on axis 0
    /// (the "cone") plus small per-item signal on the other axes — the
    /// shape ABTT is meant to fix.
    fn corpus() -> Vec<Vec<f32>> {
        (0..30)
            .map(|i| {
                let t = i as f32;
                vec![
                    10.0 + 0.1 * t,
                    (i % 5) as f32 * 0.2,
                    (i % 7) as f32 * 0.3,
                    -((i % 3) as f32) * 0.2,
                ]
            })
            .collect()
    }

    fn fill(ann: &AnnIndex, data: &[Vec<f32>]) {
        for (i, v) in data.iter().enumerate() {
            ann.upsert(&TrackId::from(format!("t{i}")), v).unwrap();
        }
    }

    #[test]
    fn has_whitening_reflects_install() {
        let ann = AnnIndex::open_in_memory(4, 16).unwrap();
        assert!(!ann.has_whitening());
        ann.set_whitening(Some(Arc::new(Whitening::fit(&corpus(), 1).unwrap())))
            .unwrap();
        assert!(ann.has_whitening());
        ann.set_whitening(None).unwrap();
        assert!(!ann.has_whitening());
    }

    #[test]
    fn set_whitening_rejects_dim_mismatch() {
        let ann = AnnIndex::open_in_memory(3, 16).unwrap();
        let w = Whitening::fit(&corpus(), 1).unwrap(); // dim 4
        assert!(matches!(
            ann.set_whitening(Some(Arc::new(w))),
            Err(AnnError::DimMismatch { .. })
        ));
    }

    #[test]
    fn query_with_raw_vector_tops_its_own_track_under_whitening() {
        let data = corpus();
        let ann = AnnIndex::open_in_memory(4, 16).unwrap();
        ann.set_whitening(Some(Arc::new(Whitening::fit(&data, 1).unwrap())))
            .unwrap();
        fill(&ann, &data);
        // Query with a *raw* vector (the same one stored as raw). Both the
        // stored vector and the query get whitened identically, so the
        // matching track tops the list at ~1.0 similarity. This is the
        // "whiten exactly once on both sides" consistency guarantee.
        let res = ann.query(&data[7], 3).unwrap();
        assert_eq!(res[0].track_id, TrackId::from("t7"));
        assert!(res[0].similarity > 0.99, "sim {}", res[0].similarity);
    }

    #[test]
    fn whitening_spreads_similarities_in_a_shared_cone() {
        let data = corpus();
        let seed = &data[0];

        let raw = AnnIndex::open_in_memory(4, 16).unwrap();
        fill(&raw, &data);
        let raw_sims: Vec<f32> = raw.query(seed, 10).unwrap().iter().map(|r| r.similarity).collect();
        // Raw vectors share a dominant axis-0 component → all near-parallel.
        assert!(
            raw_sims.iter().all(|&s| s > 0.95),
            "raw corpus not anisotropic enough: {raw_sims:?}"
        );

        let white = AnnIndex::open_in_memory(4, 16).unwrap();
        white
            .set_whitening(Some(Arc::new(Whitening::fit(&data, 1).unwrap())))
            .unwrap();
        fill(&white, &data);
        let white_sims: Vec<f32> = white
            .query(seed, 10)
            .unwrap()
            .iter()
            .map(|r| r.similarity)
            .collect();
        let white_min = white_sims.iter().copied().fold(f32::INFINITY, f32::min);
        // Removing the shared cone separates the neighbours: the tail of
        // the top-10 drops well below the raw floor.
        assert!(
            white_min < 0.95,
            "whitening did not spread similarities: {white_sims:?}"
        );
    }

    #[test]
    fn query_text_falls_back_to_audio_path_without_text_mean() {
        // With no cross-modal text mean fitted, the text query path must
        // mirror the audio query path exactly (both center by the audio
        // mean) — so an un-fitted text mean never silently changes results.
        let data = corpus();
        let ann = AnnIndex::open_in_memory(4, 16).unwrap();
        ann.set_whitening(Some(Arc::new(Whitening::fit(&data, 1).unwrap())))
            .unwrap();
        fill(&ann, &data);
        let probe = vec![3.0_f32, 0.5, 0.2, 0.0];
        let audio: Vec<_> = ann.query(&probe, 5).unwrap().into_iter().map(|r| r.track_id).collect();
        let text: Vec<_> = ann
            .query_text(&probe, 5)
            .unwrap()
            .into_iter()
            .map(|r| r.track_id)
            .collect();
        assert_eq!(audio, text);
    }

    #[test]
    fn rebuild_from_whitens_when_installed() {
        // rebuild_from is the boot path; it must whiten too, so a query
        // with a raw seed still matches its own track.
        let data = corpus();
        let ann = AnnIndex::open_in_memory(4, 16).unwrap();
        ann.set_whitening(Some(Arc::new(Whitening::fit(&data, 1).unwrap())))
            .unwrap();
        let pairs: Vec<(TrackId, Vec<f32>)> = data
            .iter()
            .enumerate()
            .map(|(i, v)| (TrackId::from(format!("t{i}")), v.clone()))
            .collect();
        ann.rebuild_from(pairs.iter().map(|(t, v)| (t, v.as_slice())))
            .unwrap();
        let res = ann.query(&data[3], 1).unwrap();
        assert_eq!(res[0].track_id, TrackId::from("t3"));
        assert!(res[0].similarity > 0.99);
    }
}
