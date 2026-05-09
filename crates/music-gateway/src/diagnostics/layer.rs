//! `tracing_subscriber::Layer` impl that turns span lifecycle events
//! into [`SpanRecord`]s, plus a background drainer that batches them
//! into the [`TraceStore`].
//!
//! Two design points worth flagging:
//!
//! 1. **Best-effort, never blocking.** Span emission must not stall a
//!    worker — even briefly. The layer pushes via `try_send`; on
//!    overflow, the span is dropped silently. We'd rather lose a
//!    handful of trace rows than have the drainer's lag propagate
//!    backpressure into request handlers.
//!
//! 2. **Trace ID = root span ID.** We don't generate UUIDs because we
//!    don't need cross-service correlation yet. The root span's
//!    numeric ID, hex-formatted, suffices for "give me every span in
//!    this ingest job."

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Subscriber, error};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use super::store::TraceStore;
use super::types::SpanRecord;

/// Layer that captures spans for the diagnostics pipeline. Construct
/// with [`TraceLayer::new`] and hand the matching `Receiver` to
/// [`spawn_drainer`] (or to a test).
#[derive(Debug)]
pub struct TraceLayer {
    sender: mpsc::Sender<SpanRecord>,
}

impl TraceLayer {
    /// Returns the layer and the receive side of its channel. The
    /// caller is responsible for either spawning a drainer or draining
    /// it directly. `buffer` is the number of in-flight records the
    /// channel will hold before `try_send` starts dropping.
    pub fn new(buffer: usize) -> (Self, mpsc::Receiver<SpanRecord>) {
        let (tx, rx) = mpsc::channel(buffer);
        (Self { sender: tx }, rx)
    }
}

/// Per-span scratch state we stash in `tracing`'s span extensions
/// during `on_new_span` and consume in `on_close`. Holds the start
/// timestamp and the accumulated field map. Field accumulation lives
/// here (rather than computed at close) so we don't have to walk the
/// span's attributes a second time.
struct SpanState {
    start_ms: i64,
    fields: serde_json::Map<String, serde_json::Value>,
}

/// `tracing` field visitor that funnels recorded values into a JSON
/// map. We accept the lossy `Debug` fallback for unknown types
/// (e.g. anything formatted via `tracing` macro `?value` syntax) —
/// the trace store is a debugging tool, not a structured log
/// pipeline, and a stringified `{:?}` value is still useful.
struct JsonVisitor<'a>(&'a mut serde_json::Map<String, serde_json::Value>);

impl Visit for JsonVisitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_string(), value.into());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().to_string(), value.into());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().to_string(), value.into());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().to_string(), value.into());
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.0.insert(field.name().to_string(), value.into());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().to_string(), format!("{value:?}").into());
    }
}

impl<S> Layer<S> for TraceLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        // `ctx.span(id)` is `None` when the registry layer isn't
        // installed; without it we have no extensions to write to.
        // Bail rather than panic.
        let Some(span) = ctx.span(id) else { return };
        let mut state = SpanState {
            start_ms: now_ms(),
            fields: serde_json::Map::new(),
        };
        attrs.record(&mut JsonVisitor(&mut state.fields));
        span.extensions_mut().insert(state);
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut ext = span.extensions_mut();
        if let Some(state) = ext.get_mut::<SpanState>() {
            values.record(&mut JsonVisitor(&mut state.fields));
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let metadata = span.metadata();

        // trace_id := root span's id (hex). For a root span this is
        // self; for a child it's the topmost ancestor. `from_root` walks
        // root → leaf so the first item is the root.
        let trace_root_id = span
            .scope()
            .from_root()
            .next()
            .map_or_else(|| id.into_u64(), |s| s.id().into_u64());
        let trace_id = format!("{trace_root_id:x}");

        let parent_span_id = span.parent().map(|p| id_to_i64(&p.id()));

        // Pull the start state out of the span; if it's gone (a layer
        // ordering bug), fall back to start=end so the record still
        // lands but with zero duration.
        let mut ext = span.extensions_mut();
        let state = ext.remove::<SpanState>();
        let end_ms = now_ms();
        let (start_ms, fields) = match state {
            Some(s) => (s.start_ms, s.fields),
            None => (end_ms, serde_json::Map::new()),
        };

        let record = SpanRecord {
            trace_id,
            span_id: id_to_i64(&id),
            parent_span_id,
            name: metadata.name().to_string(),
            target: metadata.target().to_string(),
            start_ms,
            end_ms,
            fields_json: serde_json::Value::Object(fields).to_string(),
        };

        // try_send: full → drop. We never want a span emission to wait
        // on the drainer (see module docs).
        let _ = self.sender.try_send(record);
    }
}

/// Spawn the drainer task. Reads from `rx`, batches records into the
/// store every `flush_interval`, and trims the table to `max_rows`
/// after each flush.
///
/// Exits when all senders (i.e., all `TraceLayer` clones) drop.
pub fn spawn_drainer(
    store: TraceStore,
    mut rx: mpsc::Receiver<SpanRecord>,
    flush_interval: Duration,
    max_rows: usize,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut buf: Vec<SpanRecord> = Vec::new();
        // First tick fires immediately; we skip it so empty startups
        // don't issue a no-op DELETE before any spans land.
        let mut tick = tokio::time::interval(flush_interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await;

        loop {
            tokio::select! {
                maybe = rx.recv() => {
                    if let Some(r) = maybe {
                        buf.push(r);
                    } else {
                        // Senders gone — flush whatever's left and exit.
                        flush(&store, &mut buf, max_rows).await;
                        break;
                    }
                }
                _ = tick.tick() => {
                    flush(&store, &mut buf, max_rows).await;
                }
            }
        }
    })
}

async fn flush(store: &TraceStore, buf: &mut Vec<SpanRecord>, max_rows: usize) {
    if buf.is_empty() {
        return;
    }
    let batch = std::mem::take(buf);
    if let Err(e) = store.insert_batch(batch).await {
        error!(error = %e, "diagnostics: insert_batch failed");
        return;
    }
    if let Err(e) = store.trim_to_capacity(max_rows).await {
        error!(error = %e, "diagnostics: trim_to_capacity failed");
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[allow(clippy::cast_possible_wrap)]
fn id_to_i64(id: &Id) -> i64 {
    // tracing's span ids are NonZeroU64. We store as i64 for SQLite;
    // the wrap is fine — we only need uniqueness within a single
    // process run, and `format!("{:x}", u64)` for trace_id reads the
    // u64 directly anyway.
    id.into_u64() as i64
}
