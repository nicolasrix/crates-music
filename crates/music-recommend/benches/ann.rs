//! Microbenchmarks for the `AnnIndex` (HNSW via usearch).
//!
//! Two measurement shapes:
//!
//! - **Upsert**: cost of growing the index from empty to N items. Uses
//!   `iter_batched` so each iteration starts from a fresh in-memory
//!   index — otherwise the index would keep growing across iterations
//!   and we'd bench "insert into 100 + 200 + 300 + ... items" instead
//!   of "insert N from empty".
//! - **Query**: cost of a top-k search against a pre-built index of
//!   size N. Read-only, so the same index is reused across iterations.
//!
//! Vector dim is fixed at 512 to match LAION-CLAP. Vectors are
//! deterministic pseudo-random so runs are reproducible without
//! pulling in `rand`'s `SmallRng` (one less moving part).
//!
//! Sizes (100 / 1000 / 5000) bracket the realistic library scale for
//! a self-hosted single-user setup. We don't bench 100k+ because
//! (a) it's beyond the design target and (b) each iteration would
//! take seconds, blowing past criterion's default budget.

use criterion::{BatchSize, BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use music_core::TrackId;
use music_recommend::ann::AnnIndex;

const DIM: usize = 512;
const CONNECTIVITY: usize = 16;

/// Deterministic pseudo-random vector. We don't care about
/// statistical quality — usearch only sees the bytes — so a tiny LCG
/// is fine and removes a `rand` dependency.
fn make_vector(seed: u64) -> Vec<f32> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
    (0..DIM)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            // Map to [-1, 1) by taking the high 24 bits as a signed int.
            let bits = (state >> 40) as i32;
            f32::from(bits as i16) / 32_768.0
        })
        .collect()
}

fn make_track_id(i: usize) -> TrackId {
    TrackId::from(format!("t-{i}"))
}

fn build_filled_index(n: usize) -> AnnIndex {
    let idx = AnnIndex::open_in_memory(DIM, CONNECTIVITY).expect("open ann");
    for i in 0..n {
        idx.upsert(&make_track_id(i), &make_vector(i as u64))
            .expect("upsert");
    }
    idx
}

fn bench_upsert(c: &mut Criterion) {
    let mut group = c.benchmark_group("ann_upsert_from_empty");
    // Keep the test budget tight — full curve fitting per size.
    group.sample_size(20);

    for &n in &[100usize, 1000, 5000] {
        // Pre-generate vectors so vector synthesis cost doesn't pollute
        // the timing. We're measuring the index, not the LCG.
        let vectors: Vec<(TrackId, Vec<f32>)> = (0..n)
            .map(|i| (make_track_id(i), make_vector(i as u64)))
            .collect();

        group.bench_with_input(BenchmarkId::from_parameter(n), &vectors, |b, vecs| {
            b.iter_batched(
                || AnnIndex::open_in_memory(DIM, CONNECTIVITY).expect("open ann"),
                |idx| {
                    for (id, v) in vecs {
                        idx.upsert(black_box(id), black_box(v)).expect("upsert");
                    }
                    idx
                },
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

fn bench_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("ann_query_top10");

    for &n in &[100usize, 1000, 5000] {
        let idx = build_filled_index(n);
        // Use a vector that's *in* the index — realistic "find similar
        // to seed track" pattern. Deterministic pick (middle).
        let query_vec = make_vector((n / 2) as u64);

        group.bench_with_input(
            BenchmarkId::from_parameter(n),
            &(idx, query_vec),
            |b, (idx, q)| {
                b.iter(|| {
                    let r = idx.query(black_box(q), 10).expect("query");
                    black_box(r);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_upsert, bench_query);
criterion_main!(benches);
