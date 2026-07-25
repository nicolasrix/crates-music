//! Background task that keeps the latent-space projection fresh as new
//! tracks are embedded.
//!
//! Two failure modes the design has to avoid:
//! 1. Recomputing on every drained embedding (UMAP is seconds-scale at
//!    10⁴ points; per-track is wasteful).
//! 2. Recomputing in the middle of a large ingest burst (the projection
//!    would lag the catalog and we'd burn cycles producing throwaway
//!    intermediate views).
//!
//! So the trigger is "count + quiet": only fire when (a) there are at
//! least `MIN_NEW_EMBEDDINGS` new `done` rows since the last projection
//! and (b) the count has been stable for at least `QUIET_INTERVAL`
//! (no new embeddings landed during a polling window).
//!
//! Each successful run writes under a *fresh* `proj_version` of the
//! form `auto-{unix_ms}` so the diagnostics dropdown surfaces new
//! snapshots without overwriting older ones. After each run we prune
//! to `KEEP_AUTO_RUNS` to bound disk growth.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use music_recommend::embedder::{EmbedderClient, EmbedderError, ReduceParams};
use music_recommend::projection::{Projection2D, ProjectionStore};
use music_recommend::store::EmbeddingStore;
use music_recommend::types::ModelVersion;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// Polling cadence. SQLite reads here are cheap (one COUNT(*) on an
/// indexed column); this is the latency tax between "embedding lands"
/// and "auto-recompute fires."
const CHECK_INTERVAL: Duration = Duration::from_mins(1);

/// Below this many new embeddings since the last projection, skip —
/// UMAP wall-clock cost is dominated by setup, not the marginal point,
/// so a 3-point delta isn't worth the run.
const MIN_NEW_EMBEDDINGS: u64 = 10;

/// "No new embeddings for at least this long" defines quiet. Coarse
/// debounce against active ingest bursts.
const QUIET_INTERVAL: Duration = Duration::from_secs(90);

/// How many `auto-*` proj_versions to retain per model_version. Each
/// trigger emits *two* rows (the 2D scatter and the 3D `-d3` sibling),
/// so 10 ≈ 5 trigger-pairs of history. Anything older gets pruned
/// after each successful run.
const KEEP_AUTO_RUNS: usize = 10;

/// Pattern the prune step uses to identify auto-generated
/// proj_versions. Manual runs (CLI `embedder.reduce`) don't match this
/// pattern by default and are left alone.
const AUTO_PROJ_PATTERN: &str = "auto-%";

/// Spawn the auto-projection task. Returns `None` when the embedder is
/// disabled at boot — without a reducer endpoint to call, the task has
/// nothing to do.
pub fn spawn_auto_projection_task(
    embedder: Option<EmbedderClient>,
    embedding_store: EmbeddingStore,
    projection_store: ProjectionStore,
    model_version: ModelVersion,
) -> Option<JoinHandle<()>> {
    let embedder = embedder?;
    Some(tokio::spawn(async move {
        run_loop(embedder, embedding_store, projection_store, model_version).await;
    }))
}

/// Long-running loop. Pulled out of the spawner so a test could call it
/// directly with a shorter interval; right now we only exercise the
/// stateless helpers below (`decide_action`).
async fn run_loop(
    embedder: EmbedderClient,
    embedding_store: EmbeddingStore,
    projection_store: ProjectionStore,
    model_version: ModelVersion,
) {
    // Seed `last_reduced_count` from the newest existing `auto-*` so
    // restarts don't immediately re-fire a redundant projection. If
    // there's no auto-run yet, seed at 0 — the first quiet period will
    // legitimately trigger a fresh projection (which is what a brand-
    // new gateway should do).
    let mut state = LoopState {
        last_reduced_count: initial_baseline(&projection_store, &model_version).await,
        last_seen_count: 0,
        last_change_at: Instant::now(),
    };

    let mut tick = tokio::time::interval(CHECK_INTERVAL);
    // Skip the immediate first tick — we just booted; let one CHECK_INTERVAL
    // elapse so the count has had a chance to move before deciding.
    tick.tick().await;

    loop {
        tick.tick().await;
        let current = match embedding_store.counts(&model_version).await {
            Ok(c) => c.done,
            Err(e) => {
                tracing::warn!(error = %e, "auto-projection: counts query failed");
                continue;
            }
        };
        match decide_action(
            &state,
            current,
            QUIET_INTERVAL,
            MIN_NEW_EMBEDDINGS,
            Instant::now(),
        ) {
            Action::Reduce => {
                let ts = now_unix_ms();
                let pv_2d = format!("auto-{ts}");
                let pv_3d = format!("auto-{ts}-d3");

                // Read the embedding matrix ONCE, up front, and feed the
                // same snapshot to both the 2D and 3D runs. Reading per-
                // run would (a) re-fetch + re-decode the whole matrix
                // twice and (b) let a mid-run ingest give the 3D sibling
                // a larger point set than its 2D twin — they share a
                // timestamp and are meant to describe the same snapshot.
                let embeddings = match embedding_store.list_done_embeddings(&model_version).await {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!(error = %e, "auto-projection: reading embeddings failed");
                        continue;
                    }
                };
                if embeddings.is_empty() {
                    tracing::warn!("auto-projection: no done embeddings to project");
                    continue;
                }
                let dim = embeddings[0].dim();
                let track_ids: Vec<String> = embeddings
                    .iter()
                    .map(|e| e.key.track_id.as_str().to_string())
                    .collect();
                // Consume into owned vectors — no second copy of the matrix.
                let vectors: Vec<Vec<f32>> = embeddings.into_iter().map(|e| e.vector).collect();

                tracing::info!(
                    model = %model_version,
                    proj_version_2d = pv_2d,
                    proj_version_3d = pv_3d,
                    done_count = current,
                    points = track_ids.len(),
                    new_since_last = current - state.last_reduced_count,
                    "auto-projection: triggering reduce"
                );
                // Fire 2D first, then 3D. We only advance
                // `last_reduced_count` if *both* succeed so a partial
                // failure (3D errored, 2D landed) retries on the next
                // quiet window. The wasted second 2D run is bounded —
                // it only repeats while embeddings keep landing.
                let ok_2d = run_one_reduce(
                    &embedder,
                    &projection_store,
                    &model_version,
                    &track_ids,
                    &vectors,
                    dim,
                    &pv_2d,
                    2,
                )
                .await;
                let ok_3d = run_one_reduce(
                    &embedder,
                    &projection_store,
                    &model_version,
                    &track_ids,
                    &vectors,
                    dim,
                    &pv_3d,
                    3,
                )
                .await;
                if ok_2d && ok_3d {
                    state.last_reduced_count = current;
                    if let Err(e) = projection_store
                        .prune_proj_versions(&model_version, AUTO_PROJ_PATTERN, KEEP_AUTO_RUNS)
                        .await
                    {
                        tracing::warn!(error = %e, "auto-projection: prune failed");
                    }
                }
            }
            Action::UpdateBaseline => {
                state.last_seen_count = current;
                state.last_change_at = Instant::now();
            }
            Action::Wait => {}
        }
    }
}

/// One reduce call, end to end: ship the (already-read) embedding matrix
/// to the embedder for UMAP+PCA → persist the returned coordinates under
/// `proj_version`. Returns true on success, false on any failure (logged
/// at warn level). Extracted so the 2D and 3D runs share the same
/// error-routing *and* the same in-memory matrix; the outer loop reads
/// the matrix once and decides whether both succeeded.
///
/// Vectors-over-the-wire: the gateway owns both the read (it has the
/// recommend DB) and the write (projections are derived data it
/// persists). The embedder is stateless compute in the middle — so this
/// works regardless of whether the embedder is co-located or on a
/// separate GPU host.
#[allow(clippy::too_many_arguments)]
async fn run_one_reduce(
    embedder: &EmbedderClient,
    projection_store: &ProjectionStore,
    model_version: &ModelVersion,
    track_ids: &[String],
    vectors: &[Vec<f32>],
    dim: usize,
    proj_version: &str,
    n_components: u8,
) -> bool {
    let params = ReduceParams {
        n_components,
        ..ReduceParams::default()
    };
    let points = match embedder.reduce(track_ids, vectors, dim, &params).await {
        Ok(p) => p,
        Err(EmbedderError::ModelNotLoaded) => {
            tracing::warn!(
                n_components,
                "auto-projection: embedder reports reduce extra unavailable"
            );
            return false;
        }
        Err(e) => {
            tracing::warn!(error = %e, n_components, "auto-projection: reduce call failed");
            return false;
        }
    };

    let projections: Vec<Projection2D> = points
        .into_iter()
        .map(|p| Projection2D {
            track_id: p.track_id,
            x: p.x,
            y: p.y,
            pc1: p.pc1,
            pc2: p.pc2,
            pc3: p.pc3,
            pc4: p.pc4,
            z: p.z,
        })
        .collect();

    let created_at_ms = i64::try_from(now_unix_ms()).unwrap_or(i64::MAX);
    match projection_store
        .upsert_projections(model_version, proj_version, &projections, created_at_ms)
        .await
    {
        Ok(written) => {
            tracing::info!(
                proj_version,
                written,
                n_components,
                "auto-projection: reduce complete"
            );
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, n_components, "auto-projection: writing projections failed");
            false
        }
    }
}

/// State threaded through the loop. Held out as a struct so
/// `decide_action` is pure and unit-testable without spinning up a real
/// SQLite/embedder pair.
// The `last_` prefix is semantic here — each field is the last-observed
// value of a distinct quantity — so the shared-prefix pedantic lint is a
// false positive.
#[allow(clippy::struct_field_names)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LoopState {
    last_reduced_count: u64,
    last_seen_count: u64,
    last_change_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    /// Quiet long enough + enough new embeddings — recompute.
    Reduce,
    /// The count changed since last tick; reset the quiet timer.
    UpdateBaseline,
    /// Nothing to do this tick.
    Wait,
}

/// Pure decision function. Splits "should we fire?" from the I/O so
/// tests can drive every transition deterministically.
fn decide_action(
    state: &LoopState,
    current: u64,
    quiet_interval: Duration,
    min_new: u64,
    now: Instant,
) -> Action {
    if current != state.last_seen_count {
        return Action::UpdateBaseline;
    }
    let new_since = current.saturating_sub(state.last_reduced_count);
    if new_since < min_new {
        return Action::Wait;
    }
    if now.duration_since(state.last_change_at) < quiet_interval {
        return Action::Wait;
    }
    Action::Reduce
}

async fn initial_baseline(store: &ProjectionStore, model_version: &ModelVersion) -> u64 {
    match store.proj_versions_for_model(model_version).await {
        Ok(versions) => versions
            .iter()
            .filter(|v| v.proj_version.starts_with("auto-"))
            .max_by_key(|v| v.created_at_ms)
            .map_or(0, |v| u64::try_from(v.point_count.max(0)).unwrap_or(0)),
        Err(e) => {
            tracing::warn!(error = %e, "auto-projection: initial baseline query failed");
            0
        }
    }
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(reduced: u64, seen: u64, last_change_at: Instant) -> LoopState {
        LoopState {
            last_reduced_count: reduced,
            last_seen_count: seen,
            last_change_at,
        }
    }

    #[test]
    fn updates_baseline_when_count_changed_since_last_tick() {
        let t0 = Instant::now();
        // Last we saw was 5; now it's 7. Trigger should NOT fire — the
        // count just moved, so we're still potentially in a burst.
        let action = decide_action(
            &state(0, 5, t0),
            7,
            Duration::from_secs(90),
            10,
            t0 + Duration::from_mins(1),
        );
        assert_eq!(action, Action::UpdateBaseline);
    }

    #[test]
    fn waits_when_not_enough_new_embeddings() {
        let t0 = Instant::now();
        // 8 new since last reduce, threshold is 10 — wait.
        let action = decide_action(
            &state(0, 8, t0),
            8,
            Duration::from_secs(90),
            10,
            t0 + Duration::from_secs(200),
        );
        assert_eq!(action, Action::Wait);
    }

    #[test]
    fn waits_when_quiet_interval_not_elapsed() {
        let t0 = Instant::now();
        // 12 new since last reduce (threshold 10), but only 30s since
        // the count last moved (threshold 90s). Stay put.
        let action = decide_action(
            &state(0, 12, t0),
            12,
            Duration::from_secs(90),
            10,
            t0 + Duration::from_secs(30),
        );
        assert_eq!(action, Action::Wait);
    }

    #[test]
    fn fires_when_count_stable_and_threshold_met() {
        let t0 = Instant::now();
        // 12 new since last reduce, 120s of quiet — fire.
        let action = decide_action(
            &state(0, 12, t0),
            12,
            Duration::from_secs(90),
            10,
            t0 + Duration::from_mins(2),
        );
        assert_eq!(action, Action::Reduce);
    }

    #[test]
    fn fires_at_exact_quiet_boundary() {
        // Boundary: exactly at the quiet threshold should fire. Using
        // strict `<` (not `<=`) in the implementation means equal
        // duration counts as "long enough."
        let t0 = Instant::now();
        let action = decide_action(
            &state(0, 10, t0),
            10,
            Duration::from_secs(90),
            10,
            t0 + Duration::from_secs(90),
        );
        assert_eq!(action, Action::Reduce);
    }

    #[test]
    fn waits_when_no_new_embeddings_at_all() {
        let t0 = Instant::now();
        // System is steady-state at 50 embedded tracks; both pointers
        // are at 50. No new work, no recompute.
        let action = decide_action(
            &state(50, 50, t0),
            50,
            Duration::from_secs(90),
            10,
            t0 + Duration::from_hours(1),
        );
        assert_eq!(action, Action::Wait);
    }

    #[test]
    fn handles_count_decrease_gracefully() {
        let t0 = Instant::now();
        // Embeddings were deleted / model_version changed — current is
        // *below* last_reduced. saturating_sub returns 0; we shouldn't
        // panic and we shouldn't fire.
        let action = decide_action(
            &state(100, 100, t0),
            80,
            Duration::from_secs(90),
            10,
            t0 + Duration::from_hours(1),
        );
        // Count differs from last_seen → UpdateBaseline first.
        assert_eq!(action, Action::UpdateBaseline);
    }
}
