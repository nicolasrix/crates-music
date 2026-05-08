//! In-memory shared state for sync. Wraps [`music_sync::SyncState`] in
//! an `Arc<RwLock<…>>` so multiple request handlers (and the future WS
//! fan-out task) can share it.
//!
//! Persistence is deferred — restarting the gateway resets the state
//! to default. Single-user, low-stakes; the trade-off is intentional.

use std::sync::Arc;

use music_sync::{ApplyError, SyncOp, SyncState};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Default)]
pub struct SyncStore {
    inner: Arc<RwLock<SyncState>>,
}

impl SyncStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn snapshot(&self) -> SyncState {
        self.inner.read().await.clone()
    }

    /// Apply an op under the write lock. Returns the new version on
    /// success. The lock is held for the duration of the apply, so
    /// concurrent posts are linearized.
    pub async fn apply(&self, op: &SyncOp) -> Result<u64, ApplyError> {
        let mut guard = self.inner.write().await;
        guard.apply(op)?;
        Ok(guard.version)
    }
}
