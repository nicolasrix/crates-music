//! In-memory shared state for sync. Wraps [`music_sync::SyncState`] in
//! an `Arc<RwLock<…>>` so multiple request handlers (and the WS
//! fan-out task) can share it. A `tokio::sync::broadcast` channel
//! delivers each successful op to all live WS subscribers.
//!
//! Persistence is deferred — restarting the gateway resets the state
//! to default. Single-user, low-stakes; the trade-off is intentional.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use music_core::SessionId;
use music_recommend::SessionStore;
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
    /// Persistent mirror of session lifecycle. `None` in tests that
    /// only exercise the in-memory state machine (the wider integration
    /// suite constructs a real one via `with_sessions`). Best-effort:
    /// a write failure here logs but does not roll back the in-memory
    /// apply — the broadcast anchor is the source of truth clients
    /// see live, and the session row is a reconstruction-time view.
    sessions: Option<SessionStore>,
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
            sessions: None,
        }
    }

    /// Constructor that wires up the durable session-lifecycle mirror.
    /// Production callers use this; in-memory tests of the broadcast
    /// path keep [`Self::new`] for ceremony-free setup.
    pub fn with_sessions(sessions: SessionStore) -> Self {
        let mut s = Self::new();
        s.sessions = Some(sessions);
        s
    }

    /// The currently-active recommend-session id, derived from the
    /// in-memory `SessionAnchor`. Cheap (single read-lock, no DB
    /// round-trip). Returns `None` between sessions or before the
    /// first `StartSession` of the gateway's lifetime.
    pub async fn active_session_id(&self) -> Option<SessionId> {
        let guard = self.inner.read().await;
        guard
            .playback
            .session_anchor
            .as_ref()
            .map(|a| a.session_id.clone())
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
    ///
    /// `now_ms` is stamped here from `SystemTime::now()` and threaded
    /// into the pure state machine. The state machine itself never
    /// looks at the clock — keeping it deterministic for tests.
    pub async fn apply(&self, op: &SyncOp) -> Result<u64, ApplyError> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        let mut guard = self.inner.write().await;
        // Capture the active session before apply — StopSession and
        // Clear both null `session_anchor`, so we wouldn't be able to
        // read which session id to close afterwards.
        let active_before: Option<SessionId> = guard
            .playback
            .session_anchor
            .as_ref()
            .map(|a| a.session_id.clone());
        guard.apply(op, now_ms)?;
        let version = guard.version;
        // send returns Err only if there are zero receivers — fine.
        let _ = self.bus.send(AppliedEvent {
            op: op.clone(),
            version,
        });
        drop(guard);
        self.mirror_session_lifecycle(op, active_before.as_ref(), now_ms)
            .await;
        Ok(version)
    }

    /// Persist Start/Stop/Clear into the session lifecycle table.
    /// Best-effort: a write failure logs and does not propagate, so a
    /// momentary SQLite hiccup can't break sync state — clients see
    /// the in-memory anchor, the persisted row is a reconstruction aid.
    async fn mirror_session_lifecycle(
        &self,
        op: &SyncOp,
        active_before: Option<&SessionId>,
        now_ms: i64,
    ) {
        let Some(sessions) = &self.sessions else {
            return;
        };
        match op {
            SyncOp::StartSession {
                items,
                anchor_index,
                session_id,
            } => {
                let Some(anchor_item) = items.get(*anchor_index) else {
                    return; // state.apply would have errored before us
                };
                let items_count = i64::try_from(items.len()).unwrap_or(i64::MAX);
                if let Err(err) = sessions
                    .start(session_id, &anchor_item.track_id, items_count, now_ms)
                    .await
                {
                    tracing::warn!(error = %err, session_id = %session_id.as_str(),
                        "recommend_sessions.start failed; in-memory anchor still applied");
                }
            }
            SyncOp::StopSession | SyncOp::Clear => {
                // Clear also nulls the anchor — mirror the implicit stop.
                if let Some(sid) = active_before
                    && let Err(err) = sessions.stop(sid, now_ms).await
                {
                    tracing::warn!(error = %err, session_id = %sid.as_str(),
                        "recommend_sessions.stop failed; in-memory anchor still applied");
                }
            }
            _ => {}
        }
    }
}
