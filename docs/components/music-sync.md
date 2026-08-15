# music-sync

**Path:** `crates/music-sync/`
**Type:** library, no I/O
**Test count:** 50

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

## Rooms (per-user partition)

The state machine in this crate is per-instance and identity-blind.
The **partitioning** lives in the gateway's `SyncStore`
(`music-gateway/src/sync/store.rs`): it holds a `HashMap<room_id,
RoomSync>`, where each room is one `SyncState` plus its own broadcast
bus. Every sync read/write is scoped to `principal.room_id()`:

- A **User** owns exactly one room (`room_id == their user_id`), so all
  of that user's devices share a queue — the cross-device point.
- A **Guest** owns no room; they attach to their **host's**
  (`room_id == host_user_id`, stamped on the guest principal at code
  redemption — PR D), making the host's queue a shared jukebox. A guest
  drives the same queue as the host and every other guest in that room.
- The static-bearer / legacy-token caller resolves to the owner
  (`room_id == 1`), so the pre-rooms single-queue behaviour is exactly
  the owner's room — no migration, no client change.

Rooms are created lazily on first access and a WS subscriber only
receives its own room's bus, so one User never sees another's ops. At
household scale the live-room count is the number of real accounts
(guests reuse their host's), so the map stays tiny; idle-eviction of
empty rooms is a deferred hardening, not needed yet.

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
    pub playback: PlaybackState,     // queue + cursor + position + session anchor
    pub version: u64,                // monotonic, gateway-controlled
}
```

`version` increments on every successful `apply`. Clients use it to
detect skipped messages (if you receive version 5 then version 7,
you missed 6 and should refetch the snapshot).

`playback` (from `music-core`) carries the queue, the now-playing
cursor, the play/pause flag, the head position, and — since the
recommend-session work — a `session_anchor`:

```rust
pub struct SessionAnchor {
    pub session_id: SessionId,
    pub track_id: TrackId,
    pub started_ms: i64,             // server-stamped, never client-supplied
}
```

The anchor is the "intent" tier of state: it records which
user-initiated session owns the current queue. Queue mechanics
(push/remove/reorder/cursor moves) can churn freely without touching
it — what makes a session a *session* is the user's original pick,
not what auto-fill has done to the queue since. It is skipped on the
wire when `None` (legacy snapshots without the field deserialize
fine). The recommender reads it to root autoplay refill in user
intent and to scope thumbs-down feedback to the active session.

## SyncOp

```rust
pub enum SyncOp {
    Push { item_id: QueueItemId, track_id: TrackId },
    Remove { item_id: QueueItemId },
    Reorder { item_id: QueueItemId, new_index: usize },
    SetNowPlaying { index: Option<usize> },
    SetPosition { position_ms: u64 },
    SetPlaying { is_playing: bool },
    Clear,                                            // also nulls the session anchor
    StartSession { items: Vec<QueueItem>, anchor_index: usize, session_id: SessionId },
    ReplaceUpcoming { items: Vec<QueueItem> },        // swaps the queue tail only
    StopSession,
}
```

`StartSession` / `StopSession` are the recommend-session lifetime ops:

- **`StartSession`** atomically replaces the queue with `items`, sets
  the cursor to `anchor_index`, resets position to 0, sets
  `is_playing = true`, and stamps a fresh `session_anchor`. It
  collapses what used to be a 4-op direct-play sequence (`Clear` +
  `Push`×N + `SetNowPlaying` + `SetPlaying`), so observers never see
  an inconsistent "queue swapped but no session" intermediate state.
- **`StopSession`** nulls `session_anchor` only — queue, cursor,
  position, and `is_playing` are left untouched, because stopping a
  session is an *intent* signal, not a playback command. It is
  idempotent (accepted and version-bumped even with no active
  session).

**`ReplaceUpcoming`** swaps everything *after* the cursor for `items`
and touches nothing else — the playing track keeps its position, play
state and session anchor. With no cursor it replaces the whole queue.
Ids already present in the retained prefix (or repeated inside `items`)
are dropped, so the queue can never hold one `item_id` twice, which
would make `Remove`/`Reorder` ambiguous.

It exists for the clients' play modes: turning shuffle on, turning it
off (restoring the context's own order), and mixing recommendations into
a context are all "rewrite the upcoming half". As one op the rewrite is
atomic for every observer — nobody sees a half-shuffled queue — and a
60-track album costs one frame instead of ~120 `Remove`/`Push` pairs.
Note that a *client* still needs to remember the pre-shuffle order
itself; the queue has no memory of what it was shuffled from (see the
web client's PlayModeContext).

Each op is `apply`-able to a `SyncState`:

```rust
let mut state = SyncState::default();
state.apply(&SyncOp::SetPlaying { is_playing: true }, now_ms)?;
assert_eq!(state.version, 1);
```

`apply` takes a caller-supplied `now_ms: i64` (the server's
wall-clock timestamp at apply time) and returns `Result<(),
ApplyError>`. The state machine never calls `SystemTime::now()`
itself — pushing the clock to the caller keeps `apply` pure and lets
tests replay history with synthetic time. The only field stamped from
`now_ms` is `SessionAnchor::started_ms`; every other op ignores it.
Failure cases:
- `SetNowPlaying` / `StartSession` with an out-of-range index →
  `NowPlayingOutOfBounds`.
- `StartSession` with empty `items` → `StartSessionEmpty`.

The state machine is deterministic: same starting state + same op
sequence + same `now_ms` → same result, on every device. This is the
property the "single linearizer" model relies on.

## Wire protocol

```rust
// Client → Server (WS)
pub enum ClientMessage {
    Op { op: SyncOp },
}

// Server → Client (WS only)
pub enum ServerMessage {
    Snapshot { state: SyncState },         // full state, sent on WS upgrade
    Applied { op: SyncOp, version: u64 },  // incremental broadcast
    OpError { message: String },           // sender-only rejection
}
```

There is no explicit `Subscribe` — the gateway sends a `Snapshot`
immediately after the WS upgrade, so a fresh client converges without
first calling `/v1/sync/snapshot`. `Applied` carries the op, not the
full state, so subscribers can apply it locally without comparing
every field; `version` is the post-apply counter, and a client that
sees a gap refetches the snapshot to recover. `OpError` goes only to
the sender — peers see no broadcast, since no state change happened.

## Optimistic updates on the client

Every client applies ops locally before the gateway responds, then
reconciles when the broadcast arrives:

```typescript
// Client (web)
function skip(index: number) {
  // Optimistic
  setState((s) => applyOp(s, { type: 'set_now_playing', index }));
  // Network
  api.submitOp({ type: 'set_now_playing', index }).catch(() => {
    // Rollback on failure
    refetchSnapshot();
    toast.error('Couldn’t skip — your network may be down.');
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

50 tests, all pure logic. No tokio runtime, no fixtures beyond
construct-a-state-and-apply-some-ops. Coverage:

- Every `SyncOp` variant: happy path + each `ApplyError` case.
- Version monotonicity (every `apply` increments by exactly 1).
- Idempotency where applicable (`Push` of an existing item id, and
  `StopSession` with no active session, are no-ops but still bump
  version — every applied op is a discrete event).
- Session lifetime: `StartSession` replaces the queue atomically and
  stamps `started_ms` from the `now_ms` param; `StopSession` and
  `Clear` null the anchor while the queue-mechanics ops leave it
  alone.
- Wire round-trips: `serde_json::to_string` / `from_str` for every
  message variant, including a legacy-payload deserialize test that
  proves snapshots without `session_anchor` still parse.

## Known gaps / future work

- **No conflict resolution.** Two clients submit conflicting ops
  (say, both `Reorder` the same item) simultaneously. The gateway
  processes in arrival order; whoever arrives last wins. Fine for
  cursor moves and play/pause, occasionally surprising for
  simultaneous queue edits. We may add per-field causality later.
- **No persistence**. The gateway holds each room's `SyncState` in
  memory. On restart, every room resets and all clients refetch —
  gateway uptime is the household's uptime.
- **No history**. We don't keep a list of past ops, so a client
  joining mid-stream gets a snapshot, not a replay. Acceptable
  trade-off (snapshots are small at this scale).
- **Cross-tab coordination on the web is not handled here**. Two
  open tabs both subscribed to the same WS will both apply the
  update independently. They'll converge, but the redundant work is
  visible. Service Workers (planned) will fix this.
