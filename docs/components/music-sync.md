# music-sync

**Path:** `crates/music-sync/`
**Type:** library, no I/O
**Test count:** 36

Cross-device playback sync. This crate is **pure logic**: types,
serde shapes, and the deterministic state machine. Transport (HTTP
snapshot endpoint, WebSocket fan-out) lives in `music-gateway`;
client-side optimistic UI lives in the apps.

## The problem

Three clients (laptop, phone, web) connected to the gateway. User
hits "skip" on the phone. The laptop and web should reflect that
within ~100ms without anyone noticing the network round-trip.

Solutions like CRDTs are overkill for this. We have **one
linearizer** (the gateway) — every op goes through it, gets
sequenced, and gets fanned out. So:

1. Client submits `SyncOp` over `POST /v1/sync/ops`.
2. Gateway applies op to canonical `SyncState`, increments version.
3. Gateway broadcasts the new state to every connected WebSocket.
4. Each client reconciles against its optimistic local state.

No vector clocks. No CRDTs. Just last-writer-wins with a single
linearizer.

> "This crate is pure logic: types, serde shapes, and the
> deterministic state machine. Transport lives in `music-gateway`;
> client-side optimistic UI lives in the apps."
> *— `crates/music-sync/src/lib.rs`*

## Modules

| Module | Exports |
|---|---|
| `state` | `SyncState`, `ApplyError`. The state machine. |
| `ops` | `SyncOp` enum. Every kind of mutation. |
| `wire` | `ClientMessage`, `ServerMessage`. WebSocket protocol. |

## SyncState

```rust
pub struct SyncState {
    pub version: u64,                // monotonic, gateway-controlled
    pub playback: PlaybackState,
    pub queue: Queue,
    pub likes: HashSet<TrackId>,
}
```

`version` increments on every successful `apply`. Clients use it to
detect skipped messages (if you receive version 5 then version 7,
you missed 6 and should refetch the snapshot).

## SyncOp

```rust
pub enum SyncOp {
    SetPlayback { state: PlaybackState },
    EnqueueTracks { items: Vec<QueueItem> },
    RemoveQueueItem { id: QueueItemId },
    MoveQueueItem { id: QueueItemId, to_index: usize },
    SetCurrentIndex { index: usize },
    Like { track_id: TrackId },
    Unlike { track_id: TrackId },
}
```

Each op is `apply`-able to a `SyncState`:

```rust
let mut state = SyncState::default();
state.apply(&SyncOp::Like { track_id: ... })?;
assert_eq!(state.version, 1);
```

`apply` returns `Result<(), ApplyError>`. Failure cases:
- `RemoveQueueItem` for an ID not in the queue → `NotFound`.
- `MoveQueueItem` with an out-of-range `to_index` → `OutOfRange`.
- `SetCurrentIndex` past the end of the queue → `OutOfRange`.

The state machine is deterministic: same starting state + same op
sequence → same result, on every device. This is the property the
"single linearizer" model relies on.

## Wire protocol

```rust
// Client → Server (over POST or WS)
pub enum ClientMessage {
    Subscribe { from_version: u64 },  // catch-up on connect
    Op(SyncOp),
}

// Server → Client (WS only)
pub enum ServerMessage {
    Snapshot(SyncState),               // full state, sent on Subscribe
    Update { version: u64, op: SyncOp },  // incremental
    Error { message: String },
}
```

`Update` carries the op, not the full state, so subscribers can apply
it locally without comparing every field. Clients that miss an
update (gap in versions) request a full `Snapshot` to recover.

## Optimistic updates on the client

Every client applies ops locally before the gateway responds, then
reconciles when the broadcast arrives:

```typescript
// Client (web)
function like(track_id: string) {
  // Optimistic
  setState((s) => ({ ...s, likes: new Set([...s.likes, track_id]) }));
  // Network
  api.submitOp({ type: 'Like', track_id }).catch(() => {
    // Rollback on failure
    setState((s) => removeLike(s, track_id));
    toast.error('Couldn’t save like — your network may be down.');
  });
}
```

When the WS update arrives, the client compares versions. If our
local version matches what the gateway broadcasts, no-op. If not, we
trust the gateway and overwrite local state.

> "Likes, queue reorders, skips apply locally before server ack.
> Sync layer reconciles; rollback with a toast on rejection. Never
> block on network for a UI gesture."
> *— `CLAUDE.md`*

## Tests

36 tests, all pure logic. No tokio runtime, no fixtures beyond
construct-a-state-and-apply-some-ops. Coverage:

- Every `SyncOp` variant: happy path + each `ApplyError` case.
- Version monotonicity (every `apply` increments by exactly 1).
- Idempotency where applicable (`Like` of an already-liked track is
  a no-op but still bumps version — TBD whether this is a bug or
  intentional).
- Wire round-trips: `serde_json::to_string` / `from_str` for every
  message variant.

## Known gaps / future work

- **No conflict resolution.** Two clients submit `Like(t)` and
  `Unlike(t)` simultaneously. The gateway processes in arrival order;
  whoever arrives last wins. This matches user intuition for likes
  but is inadequate for, say, queue reorders. We may add per-field
  causality later.
- **No persistence**. The gateway holds `SyncState` in memory. On
  restart, all clients have to refetch. For single-user this is
  fine — gateway uptime is the user's uptime.
- **No history**. We don't keep a list of past ops, so a client
  joining mid-stream gets a snapshot, not a replay. Acceptable
  trade-off (snapshots are small at this scale).
- **Cross-tab coordination on the web is not handled here**. Two
  open tabs both subscribed to the same WS will both apply the
  update independently. They'll converge, but the redundant work is
  visible. Service Workers (planned) will fix this.
