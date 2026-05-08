//! In-memory shared state for sync. Wraps [`music_sync::SyncState`] in
//! an `Arc<RwLock<…>>` so multiple request handlers (and the WS
//! fan-out task) can share it. A `tokio::sync::broadcast` channel
//! delivers each successful op to all live WS subscribers.
//!
//! Persistence is deferred — restarting the gateway resets the state
//! to default. Single-user, low-stakes; the trade-off is intentional.

use std::sync::Arc;

use music_sync::{ApplyError, SyncOp, SyncState};
use tokio::sync::{RwLock, broadcast};

/// Capacity of the broadcast ring. A subscriber that lags by more
/// than this many ops will see `RecvError::Lagged` and is expected to
/// resync via a fresh snapshot.
const BROADCAST_CAPACITY: usize = 256;

#[derive(Debug, Clone)]
pub struct SyncStore {
    inner: Arc<RwLock<SyncState>>,
    bus: broadcast::Sender<AppliedEvent>,
}

#[derive(Debug, Clone)]
pub struct AppliedEvent {
    pub op: SyncOp,
    pub version: u64,
}

impl Default for SyncStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncStore {
    pub fn new() -> Self {
        let (bus, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            inner: Arc::new(RwLock::new(SyncState::default())),
            bus,
        }
    }

    pub async fn snapshot(&self) -> SyncState {
        self.inner.read().await.clone()
    }

    /// Subscribe atomically with a snapshot read: the receiver is
    /// guaranteed to see every op applied *after* the snapshot, with
    /// no gap. (`broadcast::subscribe` only delivers messages sent
    /// after subscription, so registering it under the read lock —
    /// which excludes writes — pins the boundary.)
    pub async fn subscribe(&self) -> (SyncState, broadcast::Receiver<AppliedEvent>) {
        let guard = self.inner.read().await;
        let rx = self.bus.subscribe();
        let snapshot = guard.clone();
        (snapshot, rx)
    }

    /// Apply an op under the write lock. Returns the new version on
    /// success and broadcasts an `AppliedEvent` to all subscribers.
    /// The broadcast happens inside the critical section so subscribers
    /// see ops in apply order.
    pub async fn apply(&self, op: &SyncOp) -> Result<u64, ApplyError> {
        let mut guard = self.inner.write().await;
        guard.apply(op)?;
        let version = guard.version;
        // send returns Err only if there are zero receivers — fine.
        let _ = self.bus.send(AppliedEvent {
            op: op.clone(),
            version,
        });
        Ok(version)
    }
}
