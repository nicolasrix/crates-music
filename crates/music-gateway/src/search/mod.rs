//! Typo-tolerant catalog search (`GET /v1/search`).
//!
//! Navidrome's `search3` does prefix matching only, so a real typo
//! ("led zeplin") returns nothing. This module holds an in-process fuzzy
//! index over the catalog, queried with `fst` Levenshtein automata, and
//! serves a `search3`-shaped but relevance-ordered response. See
//! `docs/plans/search-typo-tolerance.md`.
//!
//! The index is a derived, in-memory cache: built at boot from Navidrome
//! and refreshed on an interval, never persisted (a rebuild is a
//! sub-second string job). Until the first build lands, the handler falls
//! back to proxying Navidrome's own `search3`.

mod catalog;
pub mod handlers;
pub mod index;
mod normalize;

use std::sync::{Arc, RwLock};
use std::time::Duration;

pub use index::{Hit, Kind, Record, SearchIndex};

use crate::config::UpstreamConfig;

/// Shared, swappable index handle held in `AppState`. Reads (queries) take
/// the read lock for microseconds; the refresh task swaps a freshly-built
/// index in under the write lock. `None` until the first build completes.
pub type SearchHandle = Arc<RwLock<Option<Arc<SearchIndex>>>>;

/// A fresh, empty handle (search degrades to the Navidrome fallback until
/// the builder fills it).
pub fn new_handle() -> SearchHandle {
    Arc::new(RwLock::new(None))
}

/// Spawn the boot-and-refresh loop: fetch the catalog, build the index,
/// swap it in, then sleep `refresh` and repeat. A zero `refresh` builds
/// once and stops. Fetch/build errors are logged and retried on the next
/// tick — the previously-built index (if any) keeps serving meanwhile.
pub fn spawn_index_builder(handle: SearchHandle, upstream: UpstreamConfig, refresh: Duration) {
    tokio::spawn(async move {
        loop {
            match catalog::fetch_records(&upstream).await {
                Ok(records) => {
                    let count = records.len();
                    match SearchIndex::build(records) {
                        Ok(index) => {
                            if let Ok(mut guard) = handle.write() {
                                *guard = Some(Arc::new(index));
                            }
                            tracing::info!(records = count, "search: index built");
                        }
                        Err(e) => tracing::warn!(error = %e, "search: index build failed"),
                    }
                }
                Err(e) => tracing::warn!(error = %e, "search: catalog fetch failed"),
            }
            if refresh.is_zero() {
                break;
            }
            tokio::time::sleep(refresh).await;
        }
    });
}
