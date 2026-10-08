# rust/log/src/lane: the capsule's lanes: transport contract, client seam, platform bridge (capsule)

Bytes in, typed frames out. The capsule's three lockstep lanes (SOM0 management, SOA0 attach, SOSV supervisor) ride a
byte transport chosen once per platform; this folder holds the contracts the leg, the supervisor and every client
program against. Part of capsule; charter: rust/log/CLAUDE.md.

## Files
- `attach_proto/`: the attach and mgmt lanes' connection and role state machine (decides; the leg executes)
- `client.rs`: the dialing seam: `Client`, `PeerIdentity`, `PeerProcess`, `Endpoint`, `PlatformEndpoint`, `map_peer_auth_outcome` (the peer-authentication mapping both connects use)
- `mod.rs`: declares the lane modules; each file gates itself by platform
- `pending.rs`: what both reapers share: `ReaperMsg` and its bounded intake, `Claimed` (one connection's joins and its `Closed`), the nonblocking `try_publish`, and the owned nonblocking `PendingJoins`
- `pipe_win/`: the Windows named-pipe transport, server and client
- `platform_transport.rs`: `PlatformTransport`, the capsule's `Transport` over `PlatformLaneServer`
- `socket_unix/`: the Unix domain-socket transport, server and client
- `test_progress.rs`: test-only socket/client and pipe progress, ownership/enqueue observations and scoped regression controls
- `reaper_tests.rs`: real-thread pending-join completion, panic, expiry and ownership tests
- `transport.rs`: transport contracts and bounds
- `wire/`: the frame layouts of the three lanes, pure encode and decode

## Start here
`transport.rs` for the contract every lane server implements; `client.rs` for how a client dials one.

## Rules
- Reapers poll every pending pair, retaining unfinished workers after expiry (`PendingJoins`); other joins use `join_within` and the caller's absolute deadline. Phase-one registered pairs use the reaper, and `join_workers` reports latched failure, including a panicked acceptor or reaper (`join_checked`).
- A normal close has its own report budget, `NORMAL_CLOSE_BUDGET` (20 s, separate from `TEARDOWN_AGGREGATE_DEADLINE`, the one absolute shutdown deadline): a pair unfinished past it is reported once and stays reaper-owned without failing the teardown, and a completed worker panic still does; a pair unfinished at the shutdown deadline fails `join_workers`.
- `TransportError::is_endpoint_absent` is the one absence predicate on every platform.
- The platform is chosen once, by `client::PlatformEndpoint` and `transport::PlatformLaneServer`.
- A connection's outbound bytes are reserved in `OutboundBudget` before queueing and released when the write returns.
- `Endpoint::drop_spare` abandons work started for a supervisor attempt that ended before a voyage dial; local endpoints use its default no-op.
