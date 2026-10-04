# rust/log/src/lane/attach_proto: the attach lane's connection and role machine (capsule)

A platform-neutral state machine over the wire frames: it decides, and the leg's run loop (`capsule/mod.rs`) executes its
`Action`s and reports the results back as events. Part of capsule; charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: the module doc, the caps and deadlines, the connection and role types and `AttachProto`'s state
- `actions.rs`: the output vocabulary: `SentMarker`, `RefusalReason`, `InputOutcome`, `Action`
- `events.rs`: `AttachProto::new`, `begin_teardown` and the lifecycle events the leg feeds back (`frame`, `sent`, `tick`, ...)
- `dispatch.rs`: pen and geometry broadcast, and the per-request handlers for mgmt and attach-client frames
- `bookkeeping.rs`: checkpoint streaming and the shared helpers (outstanding sends, close with refusal)
- `support_tests.rs`: frame builders and drivers the test files share
- `lockstep_tests.rs`: mgmt shutdown, lockstep, caps and hello tests
- `attach_tests.rs`: the pen, the ground-gated attach and snapshot slot, checkpoint streaming tests
- `liveness_tests.rs`: keepalive, progress deadline, mgmt idle deadline, queue accounting and teardown tests
- `v3_tests.rs`: owner-emitted pen and geometry tests

## Start here
`mod.rs`'s module doc, then `events.rs` `frame` and `sent`.

## Rules
- No I/O, no OS types, no clock reads: every timing-relevant method is given `now`.
- Lockstep is request-correlated: `mark_outstanding` allocates the `RequestId`; only `sent` with the matching marker
  clears it (`clear_outstanding_if_matches`). A request arriving once the reply is queued is held and replayed after
  it (`frame`, `clear_outstanding_and_replay`); any other early request closes the connection (`LockstepViolation`).
- At most `SUBSCRIBER_CAP` (4) watchers, driver included, counted from admission (`handle_attach`), and
  `NON_WATCHER_CAP` (4) connections not yet attached (`connection_opened`); an attach refused at the cap stays open.
- One checkpoint transfer at a time with one chunk in flight (`advance_checkpoint_stream`); output committed meanwhile
  queues per watcher against `WATCHER_LIVE_QUEUE_BUDGET_BYTES`, and overflow closes with no wire frame.
- Every outstanding send is bounded by `PROGRESS_DEADLINE` (30 s) in `tick`.
- Pen and geometry events reach v3 watchers only (`send_or_queue_pen_geometry`).
