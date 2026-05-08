# music-recommend

**Path:** `crates/music-recommend/`
**Type:** library, server-only
**Test count:** 54

The recommender's state layer. Owns:

1. **Embedding store** — SQLite, content-addressed by
   `(track_id, model_version)`.
2. **Ingest queue** — same SQLite table, status column doubles as
   queue.
3. **ANN index** — `usearch` HNSW, cosine metric.
4. **Embedder client** — HTTP wrapper around the Python sidecar.
5. **Ingest worker** — pulls from the queue, fetches audio, calls
   the embedder, writes to store + ANN.
6. **Event log** — append-only `events` table.

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
    ann::AnnIndex,
    ingest::{IngestWorker, AudioFetcher, rebuild_ann_from_store},
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

The store owns its own SQLite file
(`gateway-state.recommend.sqlite`), separate from the OAuth state DB.
sqlx tracks migration versions per pool — layering recommend's `0001`
onto the OAuth pool collided with OAuth's `0001`. One DB per crate
keeps each migration timeline self-contained.

## Tests

54 tests across 4 integration files (`ann.rs`, `embedder_client.rs`,
`ingest.rs`, `store.rs`) plus inline unit tests. Coverage:

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
