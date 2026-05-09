//! Microbenchmarks for `parse_server_timing`.
//!
//! The parser runs on every embed response — once we have hundreds of
//! ingestions per second, the cumulative cost matters. The benches
//! cover the three header shapes we actually see in production:
//!
//! - `stub` backend: one stage (`hash`)
//! - `clap` backend on CPU: three stages (`decode`, `resample`, `gpu_forward`)
//! - degenerate cases: empty header, malformed entries
//!
//! Black-box wraps both inputs and outputs so the optimizer can't
//! const-fold the call away.

use criterion::{Criterion, black_box, criterion_group, criterion_main};

use music_recommend::embedder::parse_server_timing;

fn bench_parser(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_server_timing");

    // Stub backend: a single stage. Most common shape in tests / dev.
    let stub = "hash;dur=0.4";
    group.bench_function("stub_single_stage", |b| {
        b.iter(|| black_box(parse_server_timing(black_box(stub))));
    });

    // CLAP on CPU: the headline case. Three stages with sub-millisecond
    // and millisecond-scale durations mixed.
    let clap_cpu = "decode;dur=12.3, resample;dur=4.5, gpu_forward;dur=2480";
    group.bench_function("clap_three_stages", |b| {
        b.iter(|| black_box(parse_server_timing(black_box(clap_cpu))));
    });

    // Degenerate but legal: extra params, weird spacing. The parser
    // should still be O(n) on the input length, no surprise blowups.
    let messy = "  decode ; desc=\"x\" ; dur=12.3 ,  resample ; dur = 4.5  ";
    group.bench_function("messy_whitespace_and_extra_params", |b| {
        b.iter(|| black_box(parse_server_timing(black_box(messy))));
    });

    // Empty input — fast path.
    group.bench_function("empty", |b| {
        b.iter(|| black_box(parse_server_timing(black_box(""))));
    });

    group.finish();
}

criterion_group!(benches, bench_parser);
criterion_main!(benches);
