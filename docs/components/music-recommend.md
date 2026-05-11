# music-recommend

**Path:** `crates/music-recommend/`
**Type:** library, server-only
**Test count:** 206

The recommender's state layer. The original "embeddings + queue + ANN"
core has grown into several focused modules; all share the same
SQLite pool (`gateway-state.recommend.sqlite`) but own independent
tables.

| Module | Owns | Migration |
|---|---|---|
| `store` | `track_embeddings` table + ingest queue (status column) | `0001_embeddings.sql` |
| `events` | `events` append-only log | `0002_events.sql` |
| `metadata` | `track_metadata` cache (artist/title/album/year/genre/duration) | `0003_track_metadata.sql` |
| `play_history` | `play_history` (track_id PK, last_played_ms) — MMR recency clock | `0004_play_history.sql` |
| `feedback` | `recommend_feedback` (track_id, session_id, vote, occurred_ms) | `0005_recommend_feedback.sql` |
| `projection` | `embedding_projection_2d` (track_id, model_version, x, y) for UMAP visualisation | `0006_embedding_projection_2d.sql` |
| `ann` | `usearch` HNSW + sidecar `(TrackId ↔ u64)` map | n/a (derived cache) |
| `embedder` | HTTP client for the Python sidecar | n/a |
| `ingest` | Worker: claim → fetch audio → embed → upsert | n/a |
| `aggregate` | Σ-similarity multi-seed fan-out helpers | n/a |
| `mmr` | Maximal Marginal Relevance reranker | n/a |
| `queue_filter` | Queue-aware diversity filter (per-artist cap, dedup, MMR) | n/a |

This crate is **server-only**. It pulls in `usearch` (ships C++),
`sqlx`, `reqwest`. Mobile clients won't link this.

## Why this design

### Content-addressed by `(track_id, model_version)`

Swapping models is non-destructive: old rows for `clap-v1` stay
queryable while new rows for `clap-v2` slowly fill in via the
background worker. No schema changes. No data loss.

> "The store is content-addressed by `(track_id, model_version)`.
> Swapping models is non-destructive: old rows remain queryable while
> the new `model_version` slowly fills in via the background ingest
> worker."
> *— `crates/music-recommend/src/lib.rs`*

### Status column doubles as queue

The `track_embeddings` table has columns `status TEXT` (`not_started`,
`in_progress`, `done`, `failed`) and `created_at`. The worker
selects `status = 'not_started'` ordered by `created_at` — that's the
queue.

```sql
SELECT track_id FROM track_embeddings
 WHERE model_version = ? AND status = 'not_started'
 ORDER BY created_at ASC
 LIMIT 1
```

This means there's no separate queue table. Adding a track to the
queue is just `INSERT OR IGNORE`. Marking it done is `UPDATE`.
Crash recovery is `UPDATE … SET status = 'not_started' WHERE status = 'in_progress'`.

The cost is that we can't represent "this is being processed by
worker N" — there's only one worker. Multi-worker would need a
proper claim-with-fencing pattern. Not yet needed.

## Public API

```rust
use music_recommend::{
    EmbeddingStore, EmbeddingKey, ModelVersion, IngestStatus,
    EmbedderClient, EmbedderConfig, EmbedderHealth,
    EventStore, EventInput, EventType,
    FeedbackStore, FeedbackAggregate, FeedbackCounts,
    PlayHistoryStore,
    ProjectionStore, Projection2D, ProjectionVersionSummary,
    MetadataStore, TrackMetadata, BackfillStats, backfill_metadata,
    MmrCandidate, mmr_rerank,
    QueueFilter, QueueFilterConfig, DiversityMode,
    ann::AnnIndex,
    aggregate::sample_indices,
    ingest::{IngestWorker, AudioFetcher, MetadataFetcher, MetadataIngest, rebuild_ann_from_store},
};
```

### `EmbeddingStore`

```rust
let store = EmbeddingStore::open(&path).await?;             // file-backed
let store = EmbeddingStore::open_in_memory().await?;        // tests

let key = EmbeddingKey::new(TrackId::from("t1"), ModelVersion::from("clap-v1"));
store.enqueue(&key).await?;                                  // INSERT OR IGNORE

let claimed = store.claim_next(&model_version).await?;       // returns Option<EmbeddingKey>
store.mark_done(&embedding).await?;
store.mark_failed(&key, "fetch failed").await?;

store.reset_failed(&model_version).await?;                   // retry all failed
store.reset_in_progress().await?;                            // boot recovery
```

### `EmbedderClient`

HTTP client for the Python sidecar.

```rust
let cfg = EmbedderConfig {
    url: "http://localhost:9000".into(),
    timeout: Duration::from_secs(30),
};
let client = EmbedderClient::new(cfg);
let health = client.healthz().await?;       // GET /healthz
let result = client.embed_audio(bytes).await?;  // POST /embed/audio
let result = client.embed_text("rainy sunday afternoon").await?;
```

`healthz` accepts both 200 and 503 — 503 means "I'm up but the model
isn't loaded." The gateway distinguishes these to surface
`ModelNotLoaded` separately from `Unreachable`.

### `AnnIndex`

```rust
let ann = AnnIndex::open(&path, dim, connectivity)?;
let ann = AnnIndex::open_in_memory(dim, connectivity)?;

ann.upsert(&track_id, &vector)?;
let results = ann.query_excluding(&seed_vector, 20, &[seed_id])?;
ann.persist()?;  // save to file (no-op for in-memory)
```

`usearch` handles HNSW + cosine internally. Two design choices we
made on top of it:

1. **String → u64 key map**. usearch keys are `u64`; our `TrackId`s
   are strings. We keep a parallel `(TrackId ↔ u64)` map in memory
   and persist it as a sidecar JSON file (`<path>.keys`). If the
   sidecar is missing or stale, the worker rebuilds it from SQLite.

2. **Expose similarity, not distance**. usearch returns
   `1 - cos_sim` (cosine distance). We invert it before returning so
   callers see `1.0 = identical, -1.0 = opposite`. Less mental
   gymnastics at call sites.

### Ingest worker

```rust
let worker = IngestWorker::new(IngestWorkerConfig {
    store: store.clone(),
    embedder: embedder.clone(),
    ann: ann.clone(),
    audio_fetcher: Arc::new(my_fetcher),
    model_version: ModelVersion::from("clap-v1"),
});

worker.process_next().await?;   // one item
let stats = worker.drain().await?;  // until queue is empty
```

`AudioFetcher` is a trait so tests can stub it. The gateway
implementation fetches a 120-second range from Navidrome via
`/rest/stream`.

### `rebuild_ann_from_store`

```rust
rebuild_ann_from_store(&store, &ann, &model_version).await?;
```

Walks every `status = 'done'` row for the given model version, reads
the vector, upserts into the ANN. Used at gateway boot when the
sidecar JSON is missing or stale, so the ANN is always
reconstructable from SQLite. The ANN is a derived cache; SQLite is
authoritative.

### `MetadataStore`

The track-metadata cache. Populated by the ingest worker (via
`MetadataIngest`) at the same time as the embedding, so by the time a
track is in the ANN we also have its artist / title / album /
duration_ms / year / genre on hand. The queue filter reads from here
on every recommend call — it cannot afford a per-track Subsonic round
trip.

```rust
let store = MetadataStore::new(embedding_pool.clone());
store.upsert(&track_id, &TrackMetadata { ... }).await?;
let row = store.get(&track_id).await?;          // Option<TrackMetadata>
let rows = store.get_many(&track_ids).await?;   // HashMap<TrackId, TrackMetadata>
```

The `backfill_metadata` helper (also reachable via the
`backfill-genre` binary in `music-gateway`) fills in genre + year for
older rows that were embedded before the metadata columns existed.

### `PlayHistoryStore`

Fast-path "when was this track last played" lookup, used by the MMR
recency penalty. One row per track, upserted by the `/rest/scrobble`
interceptor on every submission scrobble. Distinct from the event log
(which is append-only) — this table is *the* recency clock; the event
log is *all* signals.

```rust
let store = PlayHistoryStore::new(embedding_pool.clone());
store.record_play(&track_id, occurred_ms).await?;
let rows = store.last_played_many(&track_ids).await?;
```

### `FeedbackStore`

Per-session thumbs-up / thumbs-down on recommended tracks.

```rust
let store = FeedbackStore::new(embedding_pool.clone());
store.record(&track_id, "session-id", +1, occurred_ms, now_ms).await?;
store.clear(&track_id, "session-id").await?;
let counts = store.counts_for_track(&track_id).await?;
let downvotes = store.downvoted_in_session("session-id").await?;
```

`downvoted_in_session` is what the recommend handlers consult to add
session-scoped excludes to the ANN exclusion list. Downvotes from
other sessions are not consulted by design — the user's mood may have
changed.

### `ProjectionStore`

2D UMAP projection of every embedded track. Computed offline by the
`backfill-projection` workflow; read at request time by the
`/v1/diagnostics/recommend/latent_space` endpoint.

```rust
let store = ProjectionStore::new(embedding_pool.clone());
store.upsert_many(&model_version, &points).await?;
let points = store.all_for_version(&model_version).await?;
let summaries = store.list_versions().await?;
```

### Queue filter + MMR

`QueueFilter` implements the post-ANN filtering described in
[ARCHITECTURE.md](../ARCHITECTURE.md#beyond-raw-similarity):

```rust
let filter = QueueFilter::new(QueueFilterConfig {
    diversity_mode: DiversityMode::Mmr,
    mmr_lambda: 0.8,
    artist_penalty_weight: 0.15,
    max_per_artist: 0,        // hard cap; 0 disables, soft penalty does the work
    dedup_titles: true,
});
let admitted: Vec<MmrCandidate> = filter.admit(
    candidates,                  // ANN results, with vectors + metadata
    &queue_context,              // upcoming queue + now playing
    top_n,
);
```

`mmr_rerank` is the underlying primitive — pure function, no I/O, no
SQLite. The filter assembles the inputs (artist/title metadata,
last-played timestamps) and calls `mmr_rerank` with the assembled
candidate list.

### `EventStore`

```rust
let store = EventStore::new(embedding_pool.clone());
store.append_batch(&[
    EventInput {
        event_type: EventType::Scrobble,
        track_id: TrackId::from("t1"),
        occurred_at: 1_712_345_678_901,
        metadata: Some(json!({"played_ms": 180_000})),
    },
]).await?;

let count = store.count().await?;
let recent = store.recent(50).await?;
```

`EventType` has standard variants (`Scrobble`, `Skip`, `Like`,
`Unlike`, `Seek`) plus an `Other(String)` catch-all via untagged
serde. Unknown event types from clients deserialize into `Other`,
store as their literal string, round-trip back. Forward-compat for
new event kinds without a server release.

## Migrations

| Version | What |
|---|---|
| `0001_embeddings.sql` | `track_embeddings` table + indices for queue + track lookup. |
| `0002_events.sql` | `events` table + indices on `occurred_at`, `track_id`, `event_type`. |
| `0003_track_metadata.sql` | `track_metadata` cache. Columns extended over time with `year`, `genre`. |
| `0004_play_history.sql` | `play_history` (track_id PK, last_played_ms). MMR recency clock. |
| `0005_recommend_feedback.sql` | `recommend_feedback` (track_id, session_id, vote, occurred_ms). |
| `0006_embedding_projection_2d.sql` | `embedding_projection_2d` (track_id, model_version, x, y). |

The store owns its own SQLite file
(`gateway-state.recommend.sqlite`), separate from the OAuth state DB.
sqlx tracks migration versions per pool — layering recommend's `0001`
onto the OAuth pool collided with OAuth's `0001`. One DB per crate
keeps each migration timeline self-contained.

## Tests

206 tests as of last update, mostly inline unit tests on the new
post-retrieval modules (queue_filter, mmr, aggregate, metadata,
play_history, feedback, projection). Coverage at the integration
level:

- `EmbeddingStore`: enqueue, claim, mark done, mark failed, reset
  paths, idempotent insert behaviour, status counts.
- `EmbedderClient`: 200/503 differentiation, timeout handling,
  unreachable-vs-bad-response. Uses `wiremock`.
- `AnnIndex`: upsert / query / remove / persist round-trips, sidecar
  JSON corruption recovery, dimension mismatch detection.
- `IngestWorker`: end-to-end with stub fetcher + stub embedder, error
  propagation, `drain` empties the queue.
- `EventStore`: batch append, atomicity, metadata round-trip,
  forward-compat unknown event types, `recent` newest-first ordering.

## Known gaps

- **Single-worker only.** The claim path has a `BEGIN IMMEDIATE`
  transaction so two workers wouldn't race, but we haven't tested
  multi-worker. Probably needs a dedicated test.
- **No behavioural index.** Track2vec on session windows is the
  natural next step, with the event log as input. Deferred.
- **No CLAP text path wired into recommend endpoints.** The
  embedder client supports `embed_text`; the gateway doesn't expose
  a text-query endpoint yet. Deferred.
- **No re-embedding on track edit.** If track audio is replaced
  upstream, we'd serve stale embeddings. Detection requires polling
  Navidrome for ETag changes per track — feasible, not done.
