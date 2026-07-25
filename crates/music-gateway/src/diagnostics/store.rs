//! SQLite-backed ring buffer for closed `tracing` spans.
//!
//! Three operations matter on the hot path:
//!   * `insert_batch` — drainer flushes accumulated spans.
//!   * `trim_to_capacity` — bounds disk usage.
//!   * `recent(n)` — diagnostics UI reads the latest spans.
//!
//! All three are scoped to a single table; the store knows nothing
//! about model versions, embedder probes, or HTTP. Higher layers
//! compose on top.

use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use super::types::{ClientEventRecord, SpanRecord};

/// Migrations live in their own directory so they don't collide with
/// the OAuth migrations also owned by this crate. sqlx tracks the
/// applied set per-pool, but two migrators against the *same* DB file
/// would interleave — splitting directories keeps the two state
/// databases (OAuth, diagnostics) cleanly independent.
static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations-diagnostics");

#[derive(Clone, Debug)]
pub struct TraceStore {
    pool: SqlitePool,
}

impl TraceStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Open a file-backed traces DB and run migrations. WAL +
    /// busy_timeout matches the rest of the project — we expect the
    /// drainer task to be the only writer, but a future diagnostics
    /// HTTP handler may also write (e.g. browser RUM uploads).
    pub async fn open(path: &Path) -> sqlx::Result<Self> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?;
        MIGRATIONS.run(&pool).await?;
        Ok(Self { pool })
    }

    /// In-memory variant for tests. Single-connection pool because
    /// in-memory SQLite isn't shared across connections.
    pub async fn open_in_memory() -> sqlx::Result<Self> {
        let opts = SqliteConnectOptions::new()
            .in_memory(true)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        MIGRATIONS.run(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn count(&self) -> sqlx::Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM spans")
            .fetch_one(&self.pool)
            .await?;
        row.try_get::<i64, _>("n")
    }

    /// Persist a batch of closed spans in a single transaction.
    /// No-op (and not an error) on an empty batch — the drainer will
    /// flush every tick whether or not there's data.
    pub async fn insert_batch(&self, batch: Vec<SpanRecord>) -> sqlx::Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for s in &batch {
            sqlx::query(
                "INSERT INTO spans
                    (trace_id, span_id, parent_span_id, name, target,
                     start_ms, end_ms, fields_json)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&s.trace_id)
            .bind(s.span_id)
            .bind(s.parent_span_id)
            .bind(&s.name)
            .bind(&s.target)
            .bind(s.start_ms)
            .bind(s.end_ms)
            .bind(&s.fields_json)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Return the most recently inserted spans, newest first. Ordered
    /// by `id` (autoincrement) — that's "most recently received" in
    /// the layer, which matches what the diagnostics page wants. Note
    /// this is NOT ordered by `end_ms`: a long-running span that
    /// closes late but started early should still float to the top of
    /// the feed when it lands.
    pub async fn recent(&self, limit: usize) -> sqlx::Result<Vec<SpanRecord>> {
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(
            "SELECT trace_id, span_id, parent_span_id, name, target,
                    start_ms, end_ms, fields_json
             FROM spans
             ORDER BY id DESC
             LIMIT ?",
        )
        .bind(limit_i64)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(SpanRecord {
                trace_id: row.try_get("trace_id")?,
                span_id: row.try_get("span_id")?,
                parent_span_id: row.try_get("parent_span_id")?,
                name: row.try_get("name")?,
                target: row.try_get("target")?,
                start_ms: row.try_get("start_ms")?,
                end_ms: row.try_get("end_ms")?,
                fields_json: row.try_get("fields_json")?,
            });
        }
        Ok(out)
    }

    /// Filtered listing for the diagnostics UI. `name`/`since_ms` are
    /// each optional; an unset filter is a no-op rather than "match
    /// nothing", which matches how query strings tend to be supplied.
    /// Order is the same as `recent`: most-recently-received first
    /// (auto-increment `id` desc).
    pub async fn query(
        &self,
        limit: usize,
        name: Option<&str>,
        since_ms: Option<i64>,
    ) -> sqlx::Result<Vec<SpanRecord>> {
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        // Two booleans drive four arms; the SQL is short enough that
        // forking the literal beats threading conditions through a
        // string-builder. Bind order tracks `?` order in each variant.
        let rows = match (name, since_ms) {
            (None, None) => {
                sqlx::query(
                    "SELECT trace_id, span_id, parent_span_id, name, target,
                            start_ms, end_ms, fields_json
                     FROM spans
                     ORDER BY id DESC
                     LIMIT ?",
                )
                .bind(limit_i64)
                .fetch_all(&self.pool)
                .await?
            }
            (Some(n), None) => {
                sqlx::query(
                    "SELECT trace_id, span_id, parent_span_id, name, target,
                            start_ms, end_ms, fields_json
                     FROM spans
                     WHERE name = ?
                     ORDER BY id DESC
                     LIMIT ?",
                )
                .bind(n)
                .bind(limit_i64)
                .fetch_all(&self.pool)
                .await?
            }
            (None, Some(s)) => {
                sqlx::query(
                    "SELECT trace_id, span_id, parent_span_id, name, target,
                            start_ms, end_ms, fields_json
                     FROM spans
                     WHERE end_ms >= ?
                     ORDER BY id DESC
                     LIMIT ?",
                )
                .bind(s)
                .bind(limit_i64)
                .fetch_all(&self.pool)
                .await?
            }
            (Some(n), Some(s)) => {
                sqlx::query(
                    "SELECT trace_id, span_id, parent_span_id, name, target,
                            start_ms, end_ms, fields_json
                     FROM spans
                     WHERE name = ? AND end_ms >= ?
                     ORDER BY id DESC
                     LIMIT ?",
                )
                .bind(n)
                .bind(s)
                .bind(limit_i64)
                .fetch_all(&self.pool)
                .await?
            }
        };
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(SpanRecord {
                trace_id: row.try_get("trace_id")?,
                span_id: row.try_get("span_id")?,
                parent_span_id: row.try_get("parent_span_id")?,
                name: row.try_get("name")?,
                target: row.try_get("target")?,
                start_ms: row.try_get("start_ms")?,
                end_ms: row.try_get("end_ms")?,
                fields_json: row.try_get("fields_json")?,
            });
        }
        Ok(out)
    }

    /// Per-name duration histogram across the ring. SQLite computes
    /// count/min/max/sum cheaply; quantiles (p50/p95/p99) come from
    /// sorting the durations in Rust because SQLite has no built-in
    /// percentile. At our cap (100k rows) the sort is sub-millisecond.
    /// Empty buffer returns an empty Vec.
    pub async fn histogram(&self, since_ms: Option<i64>) -> sqlx::Result<Vec<HistogramBucket>> {
        // Pull (name, duration) pairs and bucket in Rust. The
        // alternative — emulating quantiles via window functions — is
        // O(n log n) on the SQLite side and harder to test.
        let rows = if let Some(s) = since_ms {
            sqlx::query("SELECT name, (end_ms - start_ms) AS dur_ms FROM spans WHERE end_ms >= ?")
                .bind(s)
                .fetch_all(&self.pool)
                .await?
        } else {
            sqlx::query("SELECT name, (end_ms - start_ms) AS dur_ms FROM spans")
                .fetch_all(&self.pool)
                .await?
        };

        let mut grouped: std::collections::HashMap<String, Vec<i64>> =
            std::collections::HashMap::new();
        for row in rows {
            let name: String = row.try_get("name")?;
            let dur: i64 = row.try_get("dur_ms")?;
            grouped.entry(name).or_default().push(dur.max(0));
        }

        let mut out: Vec<HistogramBucket> = grouped
            .into_iter()
            .map(|(name, mut durs)| {
                durs.sort_unstable();
                let count = durs.len();
                let sum: i64 = durs.iter().sum();
                #[allow(clippy::cast_precision_loss)] // count <= 100k, fits f64 exactly
                let mean_ms = sum as f64 / count as f64;
                HistogramBucket {
                    name,
                    count,
                    min_ms: *durs.first().expect("non-empty after group_by"),
                    max_ms: *durs.last().expect("non-empty after group_by"),
                    p50_ms: percentile(&durs, 50),
                    p95_ms: percentile(&durs, 95),
                    p99_ms: percentile(&durs, 99),
                    mean_ms,
                }
            })
            .collect();
        // Sort for determinism — handlers serialize this directly.
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Time-series of `(end_ms, duration_ms)` points for a single span
    /// name. Drives the per-row plot on the `/diagnostics/tracing` page,
    /// where the user expands a histogram bucket and wants to see how
    /// duration trends over time. Ordering matches [`query`]: newest
    /// (highest row id) first, so the caller can truncate to "the last
    /// N samples" by passing `limit`.
    pub async fn span_series(
        &self,
        name: &str,
        since_ms: Option<i64>,
        limit: usize,
    ) -> sqlx::Result<Vec<SpanPoint>> {
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = if let Some(s) = since_ms {
            sqlx::query(
                "SELECT end_ms, (end_ms - start_ms) AS dur_ms
                 FROM spans
                 WHERE name = ? AND end_ms >= ?
                 ORDER BY id DESC
                 LIMIT ?",
            )
            .bind(name)
            .bind(s)
            .bind(limit_i64)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT end_ms, (end_ms - start_ms) AS dur_ms
                 FROM spans
                 WHERE name = ?
                 ORDER BY id DESC
                 LIMIT ?",
            )
            .bind(name)
            .bind(limit_i64)
            .fetch_all(&self.pool)
            .await?
        };
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(SpanPoint {
                end_ms: row.try_get("end_ms")?,
                duration_ms: row.try_get::<i64, _>("dur_ms")?.max(0),
            });
        }
        Ok(out)
    }

    /// Per-direct-child aggregate for every span named `parent_name`.
    /// Computes "where did the time go inside `parent_name`?" — for
    /// each child name, count of occurrences and total wall time
    /// summed across all qualifying parent instances.
    ///
    /// One level deep: a grandchild (child-of-a-child) is *not*
    /// rolled up under the grandparent. The matching join is on both
    /// `span_id` and `trace_id` to avoid cross-trace collisions when
    /// `tracing` happens to reuse a span id across traces.
    pub async fn child_breakdown(
        &self,
        parent_name: &str,
        since_ms: Option<i64>,
    ) -> sqlx::Result<ChildBreakdown> {
        // 1. Parent stats.
        let parent_row = if let Some(s) = since_ms {
            sqlx::query(
                "SELECT COUNT(*) AS n, COALESCE(SUM(end_ms - start_ms), 0) AS s
                 FROM spans WHERE name = ? AND end_ms >= ?",
            )
            .bind(parent_name)
            .bind(s)
            .fetch_one(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT COUNT(*) AS n, COALESCE(SUM(end_ms - start_ms), 0) AS s
                 FROM spans WHERE name = ?",
            )
            .bind(parent_name)
            .fetch_one(&self.pool)
            .await?
        };
        let parent_count: i64 = parent_row.try_get("n")?;
        let parent_sum_ms: i64 = parent_row.try_get("s")?;

        // 2. Child aggregation: self-join on (span_id ↔ parent_span_id)
        // scoped by trace_id so unrelated traces with colliding ids
        // don't bleed into the count.
        let child_rows = if let Some(s) = since_ms {
            sqlx::query(
                "SELECT c.name AS name,
                        COUNT(*) AS n,
                        COALESCE(SUM(c.end_ms - c.start_ms), 0) AS s
                 FROM spans c
                 JOIN spans p
                     ON p.span_id = c.parent_span_id
                    AND p.trace_id = c.trace_id
                 WHERE p.name = ? AND p.end_ms >= ?
                 GROUP BY c.name
                 ORDER BY s DESC",
            )
            .bind(parent_name)
            .bind(s)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT c.name AS name,
                        COUNT(*) AS n,
                        COALESCE(SUM(c.end_ms - c.start_ms), 0) AS s
                 FROM spans c
                 JOIN spans p
                     ON p.span_id = c.parent_span_id
                    AND p.trace_id = c.trace_id
                 WHERE p.name = ?
                 GROUP BY c.name
                 ORDER BY s DESC",
            )
            .bind(parent_name)
            .fetch_all(&self.pool)
            .await?
        };

        let mut children = Vec::with_capacity(child_rows.len());
        for row in child_rows {
            children.push(ChildAgg {
                name: row.try_get("name")?,
                count: u64::try_from(row.try_get::<i64, _>("n")?).unwrap_or(0),
                sum_ms: row.try_get::<i64, _>("s")?.max(0),
            });
        }

        Ok(ChildBreakdown {
            parent_name: parent_name.to_string(),
            parent_count: u64::try_from(parent_count).unwrap_or(0),
            parent_sum_ms: parent_sum_ms.max(0),
            children,
        })
    }

    /// Evict oldest rows so at most `max_rows` remain. The drainer
    /// calls this after every flush. Cheap when already under
    /// capacity (single MAX(id) read + a no-op DELETE).
    pub async fn trim_to_capacity(&self, max_rows: usize) -> sqlx::Result<()> {
        let max_rows_i64 = i64::try_from(max_rows).unwrap_or(i64::MAX);
        // Subquery uses MAX(id), not COUNT(*) — autoincrement means
        // "rows survive iff their id > (max_id - max_rows)" is the
        // exact frontier we want, even after past trims.
        sqlx::query(
            "DELETE FROM spans
             WHERE id <= COALESCE((SELECT MAX(id) FROM spans), 0) - ?",
        )
        .bind(max_rows_i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Persist a batch of browser RUM events in a single transaction.
    /// Empty batch is a no-op (the upload handler returns OK with
    /// `accepted: 0` for that case). All timestamps and the user_agent
    /// must be stamped by the caller — the store stays dumb.
    pub async fn insert_client_events(&self, batch: Vec<ClientEventRecord>) -> sqlx::Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for e in &batch {
            sqlx::query(
                "INSERT INTO client_events
                    (received_ms, occurred_ms, session_id, name,
                     value_ms, rating, page_path, user_agent, fields_json)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(e.received_ms)
            .bind(e.occurred_ms)
            .bind(&e.session_id)
            .bind(&e.name)
            .bind(e.value_ms)
            .bind(e.rating.as_deref())
            .bind(&e.page_path)
            .bind(e.user_agent.as_deref())
            .bind(&e.fields_json)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Evict oldest `client_events` rows so at most `max_rows` remain.
    /// The upload handler calls this after every insert (there is no
    /// drainer for RUM — inserts come straight from the handler). Same
    /// MAX(id)-frontier strategy as [`trim_to_capacity`]: cheap and a
    /// no-op when already under capacity.
    pub async fn trim_client_events_to_capacity(&self, max_rows: usize) -> sqlx::Result<()> {
        let max_rows_i64 = i64::try_from(max_rows).unwrap_or(i64::MAX);
        sqlx::query(
            "DELETE FROM client_events
             WHERE id <= COALESCE((SELECT MAX(id) FROM client_events), 0) - ?",
        )
        .bind(max_rows_i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Most-recently-received client events, newest first. Optional
    /// name filter mirrors `query()` semantics: `None` means "all".
    pub async fn recent_client_events(
        &self,
        limit: usize,
        name: Option<&str>,
    ) -> sqlx::Result<Vec<ClientEventRecord>> {
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = match name {
            None => {
                sqlx::query(
                    "SELECT received_ms, occurred_ms, session_id, name,
                            value_ms, rating, page_path, user_agent, fields_json
                     FROM client_events
                     ORDER BY id DESC
                     LIMIT ?",
                )
                .bind(limit_i64)
                .fetch_all(&self.pool)
                .await?
            }
            Some(n) => {
                sqlx::query(
                    "SELECT received_ms, occurred_ms, session_id, name,
                            value_ms, rating, page_path, user_agent, fields_json
                     FROM client_events
                     WHERE name = ?
                     ORDER BY id DESC
                     LIMIT ?",
                )
                .bind(n)
                .bind(limit_i64)
                .fetch_all(&self.pool)
                .await?
            }
        };
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(ClientEventRecord {
                received_ms: row.try_get("received_ms")?,
                occurred_ms: row.try_get("occurred_ms")?,
                session_id: row.try_get("session_id")?,
                name: row.try_get("name")?,
                value_ms: row.try_get("value_ms")?,
                rating: row.try_get("rating")?,
                page_path: row.try_get("page_path")?,
                user_agent: row.try_get("user_agent")?,
                fields_json: row.try_get("fields_json")?,
            });
        }
        Ok(out)
    }
}

/// One row of the duration histogram returned by `TraceStore::histogram`.
/// `p*_ms` are wall-clock milliseconds, computed via nearest-rank on
/// the sorted duration slice.
/// One child-name aggregate row in a [`ChildBreakdown`]. `count` is
/// how many child spans of this name fell under any qualifying parent
/// instance; `sum_ms` is the total wall time across them. Mean follows
/// trivially from those two.
#[derive(Clone, Debug, PartialEq)]
pub struct ChildAgg {
    pub name: String,
    pub count: u64,
    pub sum_ms: i64,
}

/// Per-parent-name breakdown: how every direct child contributed to
/// the parent's total wall time. Drives the "where did the time go?"
/// stacked-bar panel on the expanded histogram row.
///
/// `parent_sum_ms - sum(children.sum_ms)` is the unaccounted residual
/// — work that happens inside the parent but isn't covered by any
/// subspan. The UI surfaces it as a shaded "self" segment.
#[derive(Clone, Debug, PartialEq)]
pub struct ChildBreakdown {
    pub parent_name: String,
    pub parent_count: u64,
    pub parent_sum_ms: i64,
    pub children: Vec<ChildAgg>,
}

/// One point on the per-span time-series plot. `end_ms` is the
/// wall-clock close time, `duration_ms` is the wall-clock duration —
/// the same numbers `query()` would surface, just stripped of
/// everything else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpanPoint {
    pub end_ms: i64,
    pub duration_ms: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HistogramBucket {
    pub name: String,
    pub count: usize,
    pub min_ms: i64,
    pub max_ms: i64,
    pub p50_ms: i64,
    pub p95_ms: i64,
    pub p99_ms: i64,
    pub mean_ms: f64,
}

/// One parsed recommend span. The handler layer aggregates many of
/// these into queue-fill / shortfall / similarity / top-results charts.
///
/// Optional fields are `None` when the underlying span was emitted
/// before the matching instrumentation landed — we keep the diagnostics
/// page resilient to old ring contents so a model-version flip doesn't
/// blank the panel for a few minutes.
#[derive(Clone, Debug, PartialEq)]
pub struct RecommendSummary {
    pub end_ms: i64,
    pub name: String,
    pub requested_n: Option<u32>,
    pub results: Option<u32>,
    pub shortfall_reason: Option<String>,
    pub result_track_ids: Vec<String>,
    pub admitted_sims: Vec<f32>,
}

impl TraceStore {
    /// Pull recommend.from_any / recommend.from_seeds spans and parse
    /// their `fields_json` into a typed summary. Ordering matches
    /// `recent`/`query` — newest first by row id.
    ///
    /// `since_ms` is an optional inclusive lower bound on `end_ms`. The
    /// row count is unbounded by design; the diagnostics surface clamps
    /// retention via `trim_to_capacity`, so the worst-case scan is
    /// already capped at the ring size (100k rows).
    pub async fn recommend_summaries(
        &self,
        since_ms: Option<i64>,
    ) -> sqlx::Result<Vec<RecommendSummary>> {
        // IN (?, ?) sidesteps an ORM and keeps the query plan obvious:
        // a table scan over the (small) ring, filtered by name and
        // optionally end_ms. No index — at 100k rows the scan is fast
        // and indexing `name` would slow the (much hotter) insert path.
        let rows = if let Some(s) = since_ms {
            sqlx::query(
                "SELECT name, end_ms, fields_json
                 FROM spans
                 WHERE name IN ('recommend.from_any', 'recommend.from_seeds')
                   AND end_ms >= ?
                 ORDER BY id DESC",
            )
            .bind(s)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT name, end_ms, fields_json
                 FROM spans
                 WHERE name IN ('recommend.from_any', 'recommend.from_seeds')
                 ORDER BY id DESC",
            )
            .fetch_all(&self.pool)
            .await?
        };

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let name: String = row.try_get("name")?;
            let end_ms: i64 = row.try_get("end_ms")?;
            let fields_json: String = row.try_get("fields_json")?;
            out.push(parse_recommend_summary(name, end_ms, &fields_json));
        }
        Ok(out)
    }
}

/// Parse one row's `fields_json` into a [`RecommendSummary`]. Missing
/// fields collapse to `None` / empty vec; a malformed `fields_json`
/// (shouldn't happen — we control the writer) also degrades to "no
/// fields" so a single bad row doesn't blank the aggregate response.
fn parse_recommend_summary(name: String, end_ms: i64, fields_json: &str) -> RecommendSummary {
    let parsed: serde_json::Value =
        serde_json::from_str(fields_json).unwrap_or(serde_json::Value::Null);
    let obj = parsed.as_object();

    let requested_n = obj
        .and_then(|o| o.get("requested_n"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|v| u32::try_from(v).ok());
    let results = obj
        .and_then(|o| o.get("results"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|v| u32::try_from(v).ok());
    let shortfall_reason = obj
        .and_then(|o| o.get("shortfall_reason"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);

    // The track-id list is stored as a JSON *string* containing a JSON
    // array. That's the cheapest way to round-trip through tracing's
    // recorder (which doesn't natively serialize Vec<String>). Parse it
    // again here, tolerating both "missing" and "malformed inner JSON".
    let result_track_ids = obj
        .and_then(|o| o.get("result_track_ids_json"))
        .and_then(serde_json::Value::as_str)
        .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
        .unwrap_or_default();

    let admitted_sims: Vec<f32> = obj
        .and_then(|o| o.get("filter_admitted_sims_json"))
        .and_then(serde_json::Value::as_str)
        .and_then(|s| serde_json::from_str::<Vec<f32>>(s).ok())
        .unwrap_or_default();

    RecommendSummary {
        end_ms,
        name,
        requested_n,
        results,
        shortfall_reason,
        result_track_ids,
        admitted_sims,
    }
}

/// Nearest-rank percentile on a pre-sorted ascending slice. `p` is in
/// [0, 100]. Empty input is rejected by the caller (`histogram` only
/// computes per-bucket after grouping, which guarantees non-empty).
fn percentile(sorted: &[i64], p: u8) -> i64 {
    debug_assert!(!sorted.is_empty(), "percentile of empty slice");
    debug_assert!(p <= 100, "percentile p must be in [0,100]");
    // Nearest-rank: index = ceil(p/100 * n) - 1, clamped to [0, n-1].
    let n = sorted.len();
    let idx = (((p as usize).saturating_mul(n)).div_ceil(100))
        .saturating_sub(1)
        .min(n - 1);
    sorted[idx]
}
