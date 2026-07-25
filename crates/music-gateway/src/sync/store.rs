//! In-memory shared state for sync, **partitioned into rooms** (PR C of
//! the user-system plan). Each room is one shared playback/queue state
//! plus its own `tokio::sync::broadcast` bus, keyed by `room_id` — a
//! User's own id, or, for a guest, their host's id (see
//! [`crate::principal::Principal::room_id`]).
//!
//! Rooms are created lazily on first access and live for the process
//! lifetime; at our scale (a household of Users) the cap is the number of
//! real accounts, which is tiny. A guest never makes a room — they attach
//! to their host's — so the bound holds even with many transient guests.
//!
//! Persistence is deferred — restarting the gateway resets every room to
//! default. Single-box, low-stakes; the trade-off is intentional.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use music_core::SessionId;
use music_recommend::SessionStore;
use music_sync::{ApplyError, SyncOp, SyncState};
use tokio::sync::{RwLock, broadcast};

/// Capacity of each room's broadcast ring. A subscriber that lags by more
/// than this many ops will see `RecvError::Lagged` and is expected to
/// resync via a fresh snapshot.
const BROADCAST_CAPACITY: usize = 256;

/// One room's live state: the authoritative queue/playback machine plus
/// the fan-out bus that delivers each applied op to that room's WS
/// subscribers. Cheap to clone — both fields are shared handles — so a
/// caller can pull a room out of the registry under a short lock, drop
/// the lock, and then do the async work on the clone.
#[derive(Debug, Clone)]
struct RoomSync {
    inner: Arc<RwLock<SyncState>>,
    bus: broadcast::Sender<AppliedEvent>,
}

impl RoomSync {
    fn new() -> Self {
        let (bus, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            inner: Arc::new(RwLock::new(SyncState::default())),
            bus,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SyncStore {
    /// Per-room live state, created lazily. The mutex is a plain blocking
    /// `std::sync::Mutex`: the only critical section is a `HashMap`
    /// get-or-insert that clones out a `RoomSync` handle — no `.await` is
    /// ever held across it, so an async mutex would cost more for nothing.
    rooms: Arc<Mutex<HashMap<i64, RoomSync>>>,
    /// Persistent mirror of session lifecycle, shared across rooms (the
    /// session table is keyed by globally-unique `session_id`). `None` in
    /// tests that only exercise the in-memory state machine; the wider
    /// integration suite constructs a real one via `with_sessions`.
    /// Best-effort: a write failure here logs but does not roll back the
    /// in-memory apply.
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
    #[must_use]
    pub fn new() -> Self {
        Self {
            rooms: Arc::new(Mutex::new(HashMap::new())),
            sessions: None,
        }
    }

    /// Constructor that wires up the durable session-lifecycle mirror.
    /// Production callers use this; in-memory tests of the broadcast
    /// path keep [`Self::new`] for ceremony-free setup.
    #[must_use]
    pub fn with_sessions(sessions: SessionStore) -> Self {
        let mut s = Self::new();
        s.sessions = Some(sessions);
        s
    }

    /// Get the room for `room_id`, creating it on first access. The lock
    /// is held only for the map lookup/insert and a cheap handle clone —
    /// never across an `.await`.
    fn room(&self, room_id: i64) -> RoomSync {
        let mut rooms = self
            .rooms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        rooms.entry(room_id).or_insert_with(RoomSync::new).clone()
    }

    /// The currently-active recommend-session id for a room, derived from
    /// its in-memory `SessionAnchor`. Cheap (single read-lock, no DB
    /// round-trip). Returns `None` between sessions or before the first
    /// `StartSession` in that room.
    pub async fn active_session_id(&self, room_id: i64) -> Option<SessionId> {
        let room = self.room(room_id);
        let guard = room.inner.read().await;
        guard
            .playback
            .session_anchor
            .as_ref()
            .map(|a| a.session_id.clone())
    }

    pub async fn snapshot(&self, room_id: i64) -> SyncState {
        let room = self.room(room_id);
        room.inner.read().await.clone()
    }

    /// Subscribe to a room atomically with a snapshot read: the receiver
    /// is guaranteed to see every op applied *after* the snapshot, with
    /// no gap. (`broadcast::subscribe` only delivers messages sent after
    /// subscription, so registering it under the read lock — which
    /// excludes writes — pins the boundary.)
    pub async fn subscribe(&self, room_id: i64) -> (SyncState, broadcast::Receiver<AppliedEvent>) {
        let room = self.room(room_id);
        let guard = room.inner.read().await;
        let rx = room.bus.subscribe();
        let snapshot = guard.clone();
        (snapshot, rx)
    }

    /// Apply an op to a room under its write lock. Returns the new version
    /// on success and broadcasts an `AppliedEvent` to that room's
    /// subscribers only. The broadcast happens inside the critical
    /// section so subscribers see ops in apply order.
    ///
    /// `now_ms` is stamped here from `SystemTime::now()` and threaded into
    /// the pure state machine. The state machine itself never looks at the
    /// clock — keeping it deterministic for tests.
    pub async fn apply(&self, room_id: i64, op: &SyncOp) -> Result<u64, ApplyError> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        let room = self.room(room_id);
        let mut guard = room.inner.write().await;
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
        let _ = room.bus.send(AppliedEvent {
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
