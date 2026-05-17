//! Microbenchmarks for `TraceStore`.
//!
//! The drainer flushes every 500ms; with 50 spans per flush that's
//! ~100 spans/sec sustained. These benches measure two things:
//!
//! - **insert_batch** at a realistic populated-table size (10k rows)
//!   with batches of 1, 50, and 200. The 50 case is the design point;
//!   1 and 200 bracket "single-span tick" and "burst after a stall".
//! - **trim_to_capacity** in two regimes: under-budget (the common
//!   case, called every flush) and over-budget (eviction path).
//!
//! Async benches use a tokio `Runtime` constructed once outside `iter`,
//! and `block_on` per iteration. The runtime construction cost would
//! otherwise dominate the measurement.
//!
//! We use an *on-disk* SQLite (tempfile) rather than `:memory:` because
//! the production drainer uses on-disk too — WAL behavior and disk
//! flush characteristics matter for the realistic number.

use std::path::PathBuf;

use criterion::{BatchSize, BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use tempfile::TempDir;
use tokio::runtime::Runtime;

use music_gateway::diagnostics::{SpanRecord, TraceStore};

const PREFILL_ROWS: usize = 10_000;
const TRIM_MAX_ROWS: usize = 100_000;

fn make_record(i: usize) -> SpanRecord {
    // Bench inputs are bounded well below i64::MAX (PREFILL_ROWS plus a
    // few hundred thousand), but `try_from` keeps clippy::pedantic
    // happy without `#[allow]` and would catch a future bench that
    // ramped the size up unexpectedly.
    let i64_i: i64 = i64::try_from(i).expect("bench index fits in i64");
    SpanRecord {
        trace_id: format!("{i:016x}"),
        span_id: i64_i,
        parent_span_id: if i.is_multiple_of(4) {
            None
        } else {
            Some(i64_i - 1)
        },
        name: "ingest.embed_one".to_string(),
        target: "music_recommend::ingest".to_string(),
        start_ms: 1_700_000_000_000 + i64_i,
        end_ms: 1_700_000_000_000 + i64_i + 12,
        fields_json: r#"{"track":"t-12345","model":"clap-htsat"}"#.to_string(),
    }
}

fn make_batch(n: usize, offset: usize) -> Vec<SpanRecord> {
    (offset..offset + n).map(make_record).collect()
}

/// Build a fresh on-disk store with `PREFILL_ROWS` already inserted.
/// Returns the temp dir guard so the file isn't unlinked before the
/// caller is done with it.
fn prefilled_store(rt: &Runtime) -> (TraceStore, TempDir) {
    let tmp = TempDir::new().expect("tempdir");
    let path: PathBuf = tmp.path().join("traces.sqlite");
    let store = rt.block_on(async {
        let s = TraceStore::open(&path).await.expect("open");
        // Prefill in chunks so we don't blow out the WAL.
        for chunk_start in (0..PREFILL_ROWS).step_by(500) {
            let chunk_end = (chunk_start + 500).min(PREFILL_ROWS);
            s.insert_batch(make_batch(chunk_end - chunk_start, chunk_start))
                .await
                .expect("prefill insert");
        }
        s
    });
    (store, tmp)
}

fn bench_insert_batch(c: &mut Criterion) {
    let rt = Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("trace_store_insert_batch");
    // Each iter does an insert + a fsync via WAL — keep sample count
    // modest so the bench wraps in a few seconds.
    group.sample_size(20);

    for &batch_size in &[1usize, 50, 200] {
        let (store, _tmp) = prefilled_store(&rt);
        let mut next_id = PREFILL_ROWS;

        group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            &batch_size,
            |b, &n| {
                b.iter_batched(
                    || {
                        let batch = make_batch(n, next_id);
                        next_id += n;
                        batch
                    },
                    |batch| {
                        rt.block_on(async {
                            store.insert_batch(black_box(batch)).await.expect("insert");
                        });
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

fn bench_trim_to_capacity(c: &mut Criterion) {
    let rt = Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("trace_store_trim");
    group.sample_size(30);

    // Hot path: drainer calls trim() every flush, almost always under
    // budget. The DELETE matches zero rows but the query still runs.
    {
        let (store, _tmp) = prefilled_store(&rt);
        group.bench_function("under_budget_noop", |b| {
            b.iter(|| {
                rt.block_on(async {
                    store
                        .trim_to_capacity(black_box(TRIM_MAX_ROWS))
                        .await
                        .expect("trim");
                });
            });
        });
    }

    // Cold path: trim down to a tighter cap. Only happens when the
    // user lowers the budget (config reload) or after a burst that
    // overran. Measured to confirm it doesn't blow up.
    {
        let (store, _tmp) = prefilled_store(&rt);
        group.bench_function("over_budget_evict_half", |b| {
            // Reset table to PREFILL_ROWS rows between iterations so
            // the eviction work is constant per-iter.
            b.iter_batched(
                || {
                    rt.block_on(async {
                        store
                            .insert_batch(make_batch(PREFILL_ROWS / 2, 999_000))
                            .await
                            .expect("refill");
                    });
                },
                |()| {
                    rt.block_on(async {
                        store
                            .trim_to_capacity(black_box(PREFILL_ROWS))
                            .await
                            .expect("trim");
                    });
                },
                BatchSize::PerIteration,
            );
        });
    }

    group.finish();
}

criterion_group!(benches, bench_insert_batch, bench_trim_to_capacity);
criterion_main!(benches);
