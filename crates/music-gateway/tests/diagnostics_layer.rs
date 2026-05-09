//! `TraceLayer` — tracing_subscriber::Layer that converts span lifecycle
//! events into `SpanRecord`s.
//!
//! These tests drive the layer with real `tracing` macros under a
//! per-test default subscriber, then drain the channel and assert.
//! They cover what the diagnostics page actually depends on:
//!   * a closed span produces exactly one record
//!   * fields recorded on the span survive as JSON
//!   * nested spans share a trace_id and link via parent_span_id

use music_gateway::diagnostics::{SpanRecord, TraceLayer, TraceStore};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::info_span;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::Registry;

/// Drain everything currently buffered in the receiver. Used after the
/// span has closed; tests exit `with_default`'s scope by dropping the
/// guard before draining so no further events arrive.
fn drain_all(rx: &mut mpsc::Receiver<SpanRecord>) -> Vec<SpanRecord> {
    let mut out = Vec::new();
    while let Ok(r) = rx.try_recv() {
        out.push(r);
    }
    out
}

#[tokio::test]
async fn closing_a_root_span_produces_one_record_with_name_and_duration() {
    let (layer, mut rx) = TraceLayer::new(64);
    let dispatch: tracing::Dispatch = Registry::default().with(layer).into();

    tracing::dispatcher::with_default(&dispatch, || {
        let span = info_span!("ingest.process_next");
        let _enter = span.enter();
        // span body — drops _enter at scope end, then span itself.
    });
    // Force span drop — _enter and span dropped above. on_close fires now.

    let records = drain_all(&mut rx);
    assert_eq!(records.len(), 1);
    let r = &records[0];
    assert_eq!(r.name, "ingest.process_next");
    assert!(r.end_ms >= r.start_ms, "end_ms must be >= start_ms");
    assert!(r.duration_ms() >= 0);
    assert_eq!(r.parent_span_id, None, "root span has no parent");
}

#[tokio::test]
async fn span_fields_round_trip_through_fields_json() {
    let (layer, mut rx) = TraceLayer::new(64);
    let dispatch: tracing::Dispatch = Registry::default().with(layer).into();

    tracing::dispatcher::with_default(&dispatch, || {
        let span = info_span!("fetch_clip", track_id = "t-7", offset_s = 515u32);
        let _enter = span.enter();
    });

    let records = drain_all(&mut rx);
    assert_eq!(records.len(), 1);
    let parsed: serde_json::Value = serde_json::from_str(&records[0].fields_json).unwrap();
    assert_eq!(parsed["track_id"], "t-7");
    assert_eq!(parsed["offset_s"], 515);
}

#[tokio::test]
async fn nested_spans_share_trace_id_and_link_via_parent() {
    let (layer, mut rx) = TraceLayer::new(64);
    let dispatch: tracing::Dispatch = Registry::default().with(layer).into();

    tracing::dispatcher::with_default(&dispatch, || {
        let outer = info_span!("outer");
        let _outer_g = outer.enter();
        {
            let inner = info_span!("inner");
            let _inner_g = inner.enter();
        }
    });

    let records = drain_all(&mut rx);
    assert_eq!(records.len(), 2, "one record per closed span");

    // on_close fires inner-first (drops first), then outer.
    let inner = records.iter().find(|r| r.name == "inner").unwrap();
    let outer = records.iter().find(|r| r.name == "outer").unwrap();

    assert_eq!(
        inner.trace_id, outer.trace_id,
        "inner inherits trace_id from outer"
    );
    assert_eq!(
        inner.parent_span_id,
        Some(outer.span_id),
        "inner.parent points at outer.span_id"
    );
    assert_eq!(outer.parent_span_id, None, "outer is the root");
}

#[tokio::test]
async fn channel_overflow_drops_spans_silently() {
    // Capacity 1: the second span won't fit. No panic, no error — we
    // just lose visibility on that span. Diagnostics is best-effort.
    let (layer, mut rx) = TraceLayer::new(1);
    let dispatch: tracing::Dispatch = Registry::default().with(layer).into();

    tracing::dispatcher::with_default(&dispatch, || {
        for i in 0..5 {
            let span = info_span!("burst", i = i);
            let _g = span.enter();
        }
    });

    let records = drain_all(&mut rx);
    assert!(
        !records.is_empty() && records.len() <= 5,
        "at least one survives, none more than emitted"
    );
}

#[tokio::test]
async fn drainer_flushes_buffered_records_to_store_on_tick() {
    // Bridge test: hand the receiver to the spawned drainer and verify
    // records land in the store within a couple of flush intervals.
    let store = TraceStore::open_in_memory().await.unwrap();
    let (layer, rx) = TraceLayer::new(64);
    let _drainer = music_gateway::diagnostics::spawn_drainer(
        store.clone(),
        rx,
        Duration::from_millis(20),
        1000,
    );

    let dispatch: tracing::Dispatch = Registry::default().with(layer).into();
    tracing::dispatcher::with_default(&dispatch, || {
        let span = info_span!("drained");
        let _g = span.enter();
    });

    // Wait long enough for at least one tick; the drainer commits in
    // a single transaction.
    let mut count = 0;
    for _ in 0..50 {
        count = store.count().await.unwrap();
        if count >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(count >= 1, "drainer should have flushed at least one row");
}
