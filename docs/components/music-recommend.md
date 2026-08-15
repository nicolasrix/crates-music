# music-recommend

**Path:** `crates/music-recommend/`
**Type:** library, server-only
**Test count:** ≈227

The recommender's state layer. The original "embeddings + queue + ANN"
core has grown into several focused modules; all share the same
SQLite pool (`gateway-state.recommend.sqlite`) but own independent
tables.

| Module | Owns | Migration |
|---|---|---|
| `store` | `track_embeddings` table + ingest queue (status column) | `0001_embeddings.sql` |
| `events` | `events` append-only log (with `session_id` stamp) | `0002_events.sql` + `0007_events_session_id.sql` |
| `metadata` | `track_metadata` cache (artist/title/album/year/genre/duration) | `0003_track_metadata.sql` |
| `play_history` | `play_history` (track_id PK, last_played_ms) — recency clock (populated by scrobble interceptor; reserved for the planned MMR recency term, see [Algorithm reference](#algorithm-reference)) | `0004_play_history.sql` |
| `feedback` | `recommend_feedback` (track_id, session_id, vote, occurred_ms) | `0005_recommend_feedback.sql` |
| `projection` | `embedding_projection_2d` (track_id, model_version, x, y, + `pc1..pc4` PCA axes, + `z` for 3D) for UMAP/PCA latent-space visualisation | `0006` + `0009_embedding_projection_pcs.sql` + `0010_embedding_projection_z.sql` |
| `whitening` | All-but-the-Top (ABTT) whitening transform — corpus mean, top-k principal directions, optional cross-modal text mean | n/a (pure compute; persisted by `whitening_store`) |
| `whitening_store` | `embedding_whitening` table (per-`model_version` fitted transform + text mean) | `0011_embedding_whitening.sql` + `0012_embedding_whitening_text_mean.sql` |
| `ann` | `usearch` HNSW + sidecar `(TrackId ↔ u64)` map; whitens vectors on entry when a transform is installed | n/a (derived cache) |
| `embedder` | HTTP client for the Python sidecar | n/a |
| `ingest` | Worker: claim → fetch audio → embed → upsert | n/a |
| `aggregate` | Σ-similarity multi-seed fan-out helpers | n/a |
| `mmr` | Maximal Marginal Relevance reranker | n/a |
| `queue_filter` | Queue-aware diversity filter (per-artist cap, dedup, MMR) | n/a |
| `sessions` | `recommend_sessions` (session_id PK, anchor_track, started/ended_ms) — durable lifecycle mirror of `SyncOp::StartSession`/`StopSession` | `0008_recommend_sessions.sql` |
| `track_affinity` | `track_affinity` (`(user_id, track_id)` PK, decayed like/skip/play counter) — feeds the gated `preference_enabled` re-scoring | `0013` → `0020_track_affinity_user_id.sql` |
| `rating` | `entity_rating` (`(user_id, kind, entity_id)` PK) — durable, always-on like/dislike for track/album/artist | `0014` → `0015` → `0021_entity_rating_user_id.sql` |
| `preference` | Pure compute: `preference_bonus`, affinity-event decay (`half_life_days_to_ms`) | n/a |
| `leash` | Pure compute: anchor-leash demotion (`LeashParams`, `nearest_anchor_sim`) for travelling stations | n/a |
| `provenance` | `recommendation` + `recommendation_item` tables — append-only log of what was served, with what scores, in what context (training substrate) | `0016` → `0022_recommendation_user_id.sql` |
| `lyrics` | `track_lyrics` (track_id PK) — resolved per-track lyrics: source, normalized `[{start_ms, text}]`, plain text, expiry. Storage only; resolution policy lives in the gateway's `lyrics/` module | `0023_track_lyrics.sql` |

This crate is **server-only**. It pulls in `usearch` (ships C++),
`sqlx`, `reqwest`. Mobile clients won't link this.

### Per-user partition (multi-user, PR E)

The taste/behaviour tables — `events`, `play_history`,
`recommend_feedback`, `track_affinity`, `entity_rating`, and
`recommendation` — each carry a `user_id` column (recommend migrations
`0017–0022`). The `WITHOUT ROWID` tables fold `user_id` into the primary
key (`play_history`/`track_affinity` → `(user_id, track_id)`,
`entity_rating` → `(user_id, kind, entity_id)`); the append-only tables
take it as an indexed column. Every store method takes a leading
`user_id: i64` and scopes its reads/writes to it. Existing rows backfill
to the owner (`DEFAULT 1`). The `user_id` is a plain integer mirroring
`gateway-state.users.id` — **no foreign key**, since this is a separate
SQLite file. Content tables (`track_embeddings`, `track_metadata`,
whitening) are intentionally *not* partitioned — embeddings are
content-addressed and shared. `track_lyrics` sits on that same shared
side: lyrics are a property of the catalog, so a per-user copy would
multiply identical rows and identical outbound requests. The gateway resolves which `user_id` to
pass (caller for writes, room host for recommendation reads); see
[music-gateway.md](./music-gateway.md#per-user-taste-isolation-pr-e).

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

## Algorithm reference

The recommend hot path is **retrieval → aggregate → filter → rerank**.
This section pins down the exact scoring used at each stage. Source of
truth is the code (`crates/music-recommend/src/{ann,aggregate,queue_filter,mmr}.rs`);
if a number here diverges from a constant in the code, the code wins.

### 1. Retrieval — cosine ANN

`AnnIndex::query` against the `usearch` HNSW (cosine metric). The vector
dimension is a per-backend property — 768 for the current CLaMP 3
embedder, 512 for the legacy CLAP one — read from config
(`[recommend].embedding_dim`), not hardcoded. Returns top-K with
`similarity ∈ [-1, 1]`. Embeddings are unit-norm, so in practice scores
cluster in `[0, 1]`.

When ABTT whitening is enabled (default), the index holds *de-coned*
vectors and `query` whitens its input by the audio mean first, so the
single rule "whiten a raw vector exactly once on entry" holds. The
text-station path uses `query_text`, which centers by the cross-modal
text mean instead (see [Whitening](#whitening-abtt) below).

### 2. Multi-seed aggregation — Σ-similarity

Used only by `POST /v1/recommend/from-seeds`. Given a list of seed
tracks, the gateway samples up to N (via `aggregate::sample_indices`,
partial Fisher–Yates) and fans out one ANN query per sampled seed.
Per-seed top-K results are folded with **weighted Σ-similarity**:

```
score(t) = Σᵢ wᵢ · cos(seedᵢ, t)         for each seed i that surfaced t
seed_hits(t) = |{i : t ∈ topK(seedᵢ)}|
```

- `wᵢ` defaults to 1.0; negative weights are clamped to 0; zero-weight
  seeds contribute nothing. Out-of-range indices in the weights slice
  fall back to 1.0.
- Sampled seed ids are added to the exclude set so a seed can't surface
  as a recommendation of itself.
- Final ranking: `score` desc, then `seed_hits` desc, then `track_id`
  asc (deterministic tiebreak).
- A track that appears under 3 seeds at similarity 0.6 each
  (`score = 1.8`) outranks one that appears under 1 seed at similarity
  0.95 (`score = 0.95`). This is the "centroid of the seed set"
  heuristic; tracks that are broadly close beat tracks that are
  laser-close to one outlier seed.

Source: `aggregate::aggregate_seed_results_weighted`.

### 3. Queue exclusion + per-`(artist, title)` dedup

`QueueFilter::build` consumes the queue snapshot (queue track ids +
now-playing) and a metadata lookup, then walks candidates with
`try_accept`. Decisions:

- **`Accept`** — survived all checks. Internal counts bump.
- **`RejectArtistCap`** — the artist's queue footprint is already at
  `max_per_artist` (`0` disables; the default disables it because the
  MMR soft penalty does the diversity work).
- **`RejectDedup`** — `(artist_key, title_normalized)` matches an entry
  already in the queue or already accepted in this call. Catches
  cross-edition duplicates (album version vs single, remaster vs
  original). `artist_key` is `"id:<artist_id>"` when available,
  `"name:<lowercased>"` otherwise.

The now-playing track *counts toward* the artist + dedup state but is
not itself excluded — its constraint applies to upcoming picks, not to
itself.

Source: `queue_filter::QueueFilter`.

### 4. MMR rerank — `λ` / `μ`

Used when `diversity_mode = "mmr"`. Greedy selection: at each step,
pick the surviving candidate maximising

```
score(c | admitted) = λ · sim(c, seed)                              (relevance)
                    − (1 − λ) · max_{a ∈ admitted} sim(c, a)        (novelty penalty)
                    − μ · artist_count(c.artist)                    (artist penalty, linear)
```

with the special case that the **first slot omits the novelty term**
(`max_sim_to_admitted` starts at 0 for everyone, and a literal
`(1 − λ)·0` would make the λ=0 case collapse to "first candidate wins"
on ties). The artist penalty still applies to the first slot, so the
opening pick already respects same-artist saturation from the queue.

Parameters:

| Symbol | Field | Range | Default | Effect |
|---|---|---|---|---|
| `λ` | `mmr_lambda` | `[0, 1]` (clamped) | `0.7` | `1.0` = pure relevance (== `DiversityMode::Off`). `0.0` = pure novelty after the relevance-driven first pick. |
| `μ` | `artist_penalty_weight` | `≥ 0` (clamped) | `0.15` | Per-occurrence cost. The 2nd same-artist admit pays `2μ`, the 3rd pays `3μ`. `μ = 0` degrades to plain MMR. |
| — | `max_per_artist` | `0..` | `0` | `0` disables the **hard** cap; the soft `μ` penalty does the diversity work. Non-zero keeps the hard cap as an emergency knob. |
| — | `dedup_titles` | bool | `true` | Runs orthogonally to `λ`/`μ` — catches cross-edition redundancy regardless of artist diversity. |

`initial_artist_counts` is seeded from the queue's existing footprint,
so a candidate that matches an artist already played twice in the
upcoming queue pays `2μ` on its *first* MMR admit.

Cosine in the novelty term uses the candidate's embedding vector.
Candidates with **no vector** (rare; only happens if the embedding
store and ANN drift out of sync) are scored on relevance alone — the
diversity penalty is treated as 0. Same fail-open posture for missing
`artist_key`.

Ties break on input order (strict `>` in argmax). Combined with the
deterministic post-aggregation sort, this makes the slate reproducible
for a given `(candidates, λ, μ, initial_counts)` tuple.

Source: `mmr::mmr_rerank`. Pure function — no I/O, no SQLite. The
queue filter assembles inputs (vectors via `AnnIndex::get_vector`,
metadata via `MetadataStore::get_many`) and calls it.

### 4b. Preference & rating re-scoring (pre-truncate)

Before the top-N truncation on the `/next` path, ANN results are nudged by
two durable, per-entity signals so a loved track sitting just outside the
raw top-N can be pulled in (`rescore_ann_by_preference` + `affinity_bonuses`
in `recommend.rs`):

- **Decayed affinity** (`preference` module, gated by
  `[recommend].preference_enabled`, default on). Each track carries a
  half-life-decayed counter (`track_affinity`) fed by likes/plays (positive)
  and skips (negative). `preference_bonus` maps `affinity ∈ [-1, 1]` to
  `β · affinity` with `β = preference_weight` (default `0.15`). Always
  *captured*; only *read* when the flag is on, so enabling it later works
  with full history.
- **Durable like/dislike** (`rating` module, **always-on**). A liked
  track/album/artist adds `like_bonus` / `like_bonus_album` /
  `like_bonus_artist` (`0.15 / 0.06 / 0.03`, `album+artist < track`); a
  dislike hard-excludes the entity's tracks from the candidate pool
  entirely (see `disliked_exclusions`). Never decays.

These two channels are deliberately separate — folding ratings into the
decaying affinity would let a dislike fade and would double-count. See
`RatingStore` / `TrackAffinityStore` below.

**Bonus units.** Every bonus above is tuned on the **cosine** scale — each
reads as "treat this candidate as if it measured `bonus` more similar". That
is directly addable on the single-seed paths (`/next`, `from-any`), whose
score *is* one cosine. `from-seeds` is not: it scores `Σ wᵢ·simᵢ`, so the
gateway multiplies the whole bonus map by `seed_weight_total` (the summed
weight of the seeds that produced results) before applying it. The identity
is just distributivity — adding `b` to every seed's similarity adds `b·Σwᵢ`
to the score.

Skipping that conversion does not merely weaken preference on the autoplay
path, it inverts it: the tethered-drift client weights anchors at `3.0` and
frontier-tail seeds at `~0.05`, so a flat bonus barely moves a candidate
several heavy seeds agree on while swamping one surfaced by a single faint
frontier seed — strongest exactly where the acoustic evidence is weakest.
The factor is summed over *all* queried seeds, not per candidate, so it stays
request-constant; scaling it per candidate would instead scale with
centrality and amplify the candidates the aggregation is already most sure
of. Pinned by `from_seeds_scales_preference_bonus_into_sigma_similarity_units`.

### 4c. Anchor leash — travelling stations

`/next` (and seed stations) accept an optional `anchor_track_ids` list. When
present, every aggregated candidate is demoted by

```
penalty(c) = λ · max(0, τ − sim(c, nearest_anchor))²        (whitened cosine)
```

before the final top-N walk (`leash::LeashParams::apply`). This keeps a
*travelling* autoplay station tethered near the user's actual picks while it
explores — candidates that drift past `τ` cosine of the nearest anchor are
pulled back. `τ` / `λ` come from `[recommend].leash_tau` / `leash_lambda`
(defaults `0.28` / `16`), with per-request overrides from the web Settings
page; `λ ≤ 0` disables it (legacy behaviour). A low-weight **recency
frontier** seed steers direction without being added to the anchor set, so
it biases travel without widening the leash.

Source: `leash::{LeashParams, nearest_anchor_sim, apply}`.

### 5. Session-scoped downvotes

When the client supplies `session_id` and the user has thumbs-downed
tracks within *that* session, those track ids are added to the queue
exclusion set before candidates are scored. Per-session, not global —
the user may have been in a different mood last week.

Source: `feedback::FeedbackStore::downvoted_in_session`, consumed in
the recommend handlers.

### Planned: recency penalty

The `play_history` table is populated today (scrobble interceptor
upserts `MAX(last_played_ms, …)` per track), but the MMR scorer does
**not** yet read it. The intent is an additional term like

```
− ρ · recency_decay(now − last_played_ms)
```

that demotes recently-played tracks without scanning the full event
log on every call. Tracked as a follow-up; the table + index are in
place so the wiring is a `last_played_for_many` lookup plus one extra
penalty term in the score formula.

Source: `play_history::PlayHistoryStore`. Not yet imported by `mmr.rs`.

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
    Whitening, WhiteningStore, default_k,
    RatingStore, RatedKind, Rating,
    TrackAffinityStore, AffinityRow, AffinityEvent, preference_bonus,
    LeashParams, LeashCandidate, LeashStats,
    RecommendationLogStore, RecommendationRecord, RecommendationItemRecord,
    RecommendationKind, RecommendationOutcome, StoredRecommendation,
    LyricsStore, LyricsRow, LyricsSource, LyricLine, MatchKind,
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

// Bulk form: one transaction, returns how many rows were *actually*
// inserted. That delta is what the gateway's catalog watcher logs, and
// it's why re-offering the whole catalog every sweep is cheap and safe.
let new_count = store.enqueue_many(&track_ids, &model_version).await?;

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

ann.set_whitening(Some(whitening.into()))?;   // install ABTT transform; None = identity
ann.upsert(&track_id, &vector)?;               // whitens raw vector on entry
let results = ann.query_excluding(&seed_vector, 20, &[seed_id])?;  // audio-mean centering
let station = ann.query_text(&text_vector, 20)?;                   // text-mean centering
ann.persist()?;  // save to file (no-op for in-memory)
```

`usearch` handles HNSW + cosine internally. Three design choices we
made on top of it:

1. **String → u64 key map**. usearch keys are `u64`; our `TrackId`s
   are strings. We keep a parallel `(TrackId ↔ u64)` map in memory
   and persist it as a sidecar JSON file (`<path>.keys`). If the
   sidecar is missing or stale, the worker rebuilds it from SQLite.

2. **Expose similarity, not distance**. usearch returns
   `1 - cos_sim` (cosine distance). We invert it before returning so
   callers see `1.0 = identical, -1.0 = opposite`. Less mental
   gymnastics at call sites.

3. **Whitening lives inside the index**. When a `Whitening` transform
   is installed (`set_whitening`), `upsert`/`rebuild_from`/`query`
   whiten the raw vector exactly once on entry, so the HNSW stores
   de-coned vectors and MMR's candidate-vs-candidate cosine reads the
   already-whitened stored vector (no caller re-whitens). `query_text`
   is the one exception: it centers by the cross-modal text mean rather
   than the audio mean, because text queries sit at a modality-gap
   offset. `None` = identity, so the `whitening_enabled = false` path
   behaves exactly as before whitening existed. `set_whitening` does
   **not** retroactively re-whiten — follow it with `rebuild_from`.

### Whitening (ABTT)

CLaMP 3 embeddings are **anisotropic** — they occupy a narrow cone, so
raw cosines are inflated and poorly separated (worst for text-query
stations, where unrelated prompts can collapse together). All-but-the-Top
whitening de-cones them:

1. Subtract the corpus **mean** (the dominant shared direction).
2. Project out the top `k ≈ dim/100` **principal directions**
   (power-iteration + deflation — no linear-algebra dependency).
3. Renormalize to unit length.

```rust
let w = Whitening::fit(&audio_vectors, default_k(dim))?;     // fit over the corpus
let w = w.with_text_mean(text_modality_mean)?;               // optional cross-modal mean
let whitened = w.transform(&raw_audio_vec)?;                 // audio path (audio mean)
let whitened = w.transform_text(&raw_text_vec)?;             // station path (text mean)
```

The transform is fit **post-hoc over existing audio embeddings — no
re-embedding** — and lives inside `AnnIndex` (see above), so the single
invariant "whiten a raw vector exactly once on entry" holds and the only
consumer change is that seed lookups return the raw SQLite row.

**Cross-modal text mean.** ABTT is fit on audio, but CLaMP 3 *text*
embeddings sit at a modality-gap offset; centering them by the audio mean
collapses station queries. So `Whitening` carries an optional `text_mean`
(estimated by embedding a fixed prompt corpus through the sidecar);
`query_text` centers by it before the shared de-coning. The text mean only
affects queries, so attaching it needs **no ANN rebuild**.

`WhiteningStore` persists the fitted transform per `model_version`
(`embedding_whitening` table). The gateway fits-or-loads at boot, gated by
`[recommend].whitening_enabled` (default true), and refits on demand via
`POST /v1/recommend/refit_whitening`.

> Note: with the text tokenizer fixed upstream, station collapse is
> resolved even with whitening off; the audio-fit de-coning applied to the
> *text* path is marginally worse for genre purity than leaving text
> un-de-coned — a tracked follow-up to route the station path around
> de-coning. The audio whitening that drives `/next` stays as-is.

Source: `whitening::Whitening`, `whitening_store::WhiteningStore`,
`whitening_text` (gateway-side text-mean fit).

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

### `RatingStore`

The user's **durable like/dislike** for any rateable entity — a track,
album, or artist. Backed by one generic `entity_rating(kind, entity_id,
rating, updated_ms)` table (migration `0015`). This is a separate channel
from both `FeedbackStore` (session-scoped recommendation thumbs) and
`TrackAffinityStore` (decaying play/skip affinity): ratings never decay and
are enforced always-on, so folding them into affinity would double-count
and let dislikes fade. See `src/rating.rs` for the full rationale.

```rust
let store = RatingStore::new(embedding_pool.clone());
store.set(RatedKind::Album, "al-123", Rating::Dislike, now_ms).await?;
store.clear(RatedKind::Artist, "ar-2").await?;
let verdict = store.get(RatedKind::Track, "tr-9").await?;        // Option<Rating>
let disliked = store.disliked_ids(RatedKind::Album).await?;       // HashSet<String>
let liked    = store.liked_ids(RatedKind::Artist).await?;         // Vec<String>
let all      = store.all().await?;        // Vec<(RatedKind, String, Rating)>, newest-first
```

How the recommend handlers consume it (per-request, in `recommend.rs`):

- **Dislike → exclusion.** `disliked_exclusions` unions disliked track ids
  with the tracks of disliked albums/artists (expanded via
  `MetadataStore::track_ids_for_albums` / `track_ids_for_artists`) into the
  ANN exclude set every recommend path already honours.
- **Like → additive bonus.** `affinity_bonuses` adds `LIKE_BONUS` to liked
  tracks, plus `LIKE_BONUS_ALBUM` / `LIKE_BONUS_ARTIST` to candidates whose
  album/artist is liked. The constants descend `0.15 > 0.06 > 0.03` with
  `album + artist < track`, so a directly-liked track always outranks one
  liked only through its parents. The magnitudes are config-overridable
  (`[recommend] like_bonus*`).

### `TrackAffinityStore`

The decaying play/skip/like **affinity** counter — one row per track,
half-life-decayed (`affinity_half_life_days`, default 30). Distinct from
`RatingStore` (durable, never-decay) and `FeedbackStore` (session-scoped).
Feeds the `preference_enabled` re-scoring; always written, only read when
the flag is on.

```rust
let store = TrackAffinityStore::new(embedding_pool.clone());
store.apply_event(&track_id, AffinityEvent::Like, now_ms).await?;   // +signal
store.apply_event(&track_id, AffinityEvent::Skip, now_ms).await?;   // −signal
let aff = store.affinity_many(&track_ids, now_ms).await?;  // HashMap<TrackId, f32> in [-1,1]
```

`preference_bonus(affinity, weight)` (pure fn) maps the decayed value to the
`β · affinity` relevance nudge applied in `recommend.rs`.

### `RecommendationLogStore`

Append-only **provenance** of what the recommender served — one
`recommendation` row per served request (the context: kind, seed, session,
served_ms) plus one `recommendation_item` row per served candidate (entity
id, rank, score, optional feature JSON). The training substrate for future
learning-to-rank models; it has **no effect on what gets recommended**.

```rust
let store = RecommendationLogStore::new(embedding_pool.clone());
store.record(&RecommendationRecord {
    kind: RecommendationKind::Next,
    session_id, seed_track_id, served_ms,
    items: vec![RecommendationItemRecord::new("tr-1", Some(0.83))
        .with_features(json!({ "affinity_bonus": 0.15 }))],
    /* … */
}).await?;
let recent = store.recent(50).await?;
let labelled = store.recent_with_outcomes(50).await?;   // joins events at read time
```

Outcomes (skip/play/like) are **not** stored here — they're joined in from
the event log at training time (`recent_with_outcomes`), preserving the
write-once-at-serve-time property. Write is best-effort: a failed log warns
and never blocks serving. Gated by `[recommend].log_provenance` (default on).
Surfaced read-only at `GET /v1/diagnostics/recommendations`.

### `ProjectionStore`

UMAP/PCA projection of every embedded track for the latent-space
visualisation, read at request time by the
`/v1/diagnostics/recommend/latent_space` endpoint. Each `Projection2D`
row carries `x, y` (UMAP), optional `z` (3D UMAP, migration `0010`), and
optional `pc1..pc4` (PCA axes, migration `0009`).

```rust
let store = ProjectionStore::new(embedding_pool.clone());
store.upsert_many(&model_version, &points).await?;
let points = store.all_for_version(&model_version).await?;
let summaries = store.list_versions().await?;
```

Reduction is **vectors-over-the-wire** (it does not require a shared
filesystem): the gateway reads its own embedding rows, ships the `(N,
dim)` matrix to the embedder's `POST /reduce` (base64 row-major LE f32),
and persists the returned coordinates itself. This is what lets the
embedder run on a separate GPU host from the gateway. `auto_projection`
(gateway-side) drives both a 2D `auto-{ts}` and a 3D `auto-{ts}-d3`
version after each ingest drain.

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
        session_id: Some(SessionId::from("s-abc")),  // optional; NULL if absent
    },
]).await?;

let count = store.count().await?;
let recent = store.recent(50).await?;
let by_sess = store.by_session(&SessionId::from("s-abc"), 100).await?;
let counts = store.count_events_per_session(&[sid1, sid2]).await?;
```

`EventType` has standard variants (`Scrobble`, `Skip`, `Like`,
`Unlike`, `Seek`) plus an `Other(String)` catch-all via untagged
serde. Unknown event types from clients deserialize into `Other`,
store as their literal string, round-trip back. Forward-compat for
new event kinds without a server release.

`session_id` is optional and round-trips as SQL NULL when absent.
Populated by the gateway's `/rest/scrobble` interceptor with the
active recommend-session at write time; lets per-session
reconstruction (`by_session`) find scrobbles, skips and seeks that
fired while the user was listening to a given queue.

### `SessionStore`

Durable mirror of `SyncOp::StartSession` / `SyncOp::StopSession`.
The in-memory `SessionAnchor` in `music_sync::SyncState` is a view of
whichever row is currently open (`ended_ms IS NULL`); this store is
the persisted source of truth that survives gateway restarts.

```rust
let store = SessionStore::new(embedding_pool.clone());
store.start(&sid, &anchor_track, items_count, started_ms).await?;
// later:
store.stop(&sid, ended_ms).await?;

let active = store.active().await?;       // Option<SessionRow>; ≤ 1 by invariant
let row    = store.get(&sid).await?;
let recent = store.recent(50).await?;     // newest started_ms first
```

`start` closes any currently-active row at the new `started_ms`
before inserting the new one — sync state allows one active session
at a time, and this store mirrors that. `SyncStore::with_sessions`
wires the dispatch in production; `SyncStore::new()` keeps tests of
the broadcast path zero-dependency.

## Migrations

| Version | What |
|---|---|
| `0001_embeddings.sql` | `track_embeddings` table + indices for queue + track lookup. |
| `0002_events.sql` | `events` table + indices on `occurred_at`, `track_id`, `event_type`. |
| `0003_track_metadata.sql` | `track_metadata` cache. Columns extended over time with `year`, `genre`. |
| `0004_play_history.sql` | `play_history` (track_id PK, last_played_ms). MMR recency clock. |
| `0005_recommend_feedback.sql` | `recommend_feedback` (track_id, session_id, vote, occurred_ms). |
| `0006_embedding_projection_2d.sql` | `embedding_projection_2d` (track_id, model_version, x, y). |
| `0007_events_session_id.sql` | `ALTER TABLE events ADD COLUMN session_id TEXT` + partial index. NULL allowed (pre-0007 rows, out-of-session events, missing client payload). |
| `0008_recommend_sessions.sql` | `recommend_sessions` (session_id PK, anchor_track_id, items_count, started_ms, ended_ms). Partial index on `ended_ms IS NULL` for O(1) active-session lookup. |
| `0009_embedding_projection_pcs.sql` | Adds `pc1..pc4` PCA-axis columns to `embedding_projection_2d`. |
| `0010_embedding_projection_z.sql` | Adds `z` column for 3D UMAP projections (`auto-{ts}-d3`). |
| `0011_embedding_whitening.sql` | `embedding_whitening` (per-`model_version` ABTT transform: mean + top-k principal directions + fit metadata). |
| `0012_embedding_whitening_text_mean.sql` | Adds the cross-modal `text_mean` column for centering text-station queries. |
| `0013_track_affinity.sql` | `track_affinity` (track_id PK, decayed counter + last-update ms). Feeds the `preference_enabled` re-scoring. |
| `0014_track_rating.sql` | `track_rating` (track_id PK, ±1 verdict, updated_ms) — the original track-only durable like/dislike. Superseded by `0015`. |
| `0015_entity_rating.sql` | Generalises ratings to any entity: `entity_rating(kind, entity_id, rating, updated_ms)`, `WITHOUT ROWID`, PK `(kind, entity_id)`. Copies the live `track_rating` rows forward as `kind='track'`, then drops `track_rating`. Forward-only. |
| `0016_recommendation_log.sql` | Provenance: `recommendation` (one row per served request — context) + `recommendation_item` (one row per served candidate — slate, score, features). Append-only, write-once-at-serve. Outcomes joined from the event log at training time, not stored here. |

The store owns its own SQLite file
(`gateway-state.recommend.sqlite`), separate from the OAuth state DB.
sqlx tracks migration versions per pool — layering recommend's `0001`
onto the OAuth pool collided with OAuth's `0001`. One DB per crate
keeps each migration timeline self-contained.

## Tests

≈227 tests as of last update, mostly inline unit tests on the
post-retrieval modules (queue_filter, mmr, aggregate, metadata,
play_history, feedback, projection, sessions). Coverage at the
integration level:

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
- **Text-station handler is filter-bypass.** `GET /v1/recommend/station`
  embeds the prompt and runs `AnnIndex::query_text` top-N — that's it.
  No queue context, no MMR rerank, no per-session downvote exclusion.
  The same `EmbedderClient::embed_text` + `query_text` primitives used
  here are wired through; layering the queue filter onto the text path
  is a small follow-up.
- **Text path uses audio-fit de-coning.** The ABTT components are fit on
  audio; applying them to text (after text-mean centering) is marginally
  worse for genre purity than leaving text un-de-coned. Routing the
  station query path around de-coning is a tracked follow-up.
- **No re-embedding on track edit.** If track audio is replaced
  upstream, we'd serve stale embeddings. Detection requires polling
  Navidrome for ETag changes per track — feasible, not done.
