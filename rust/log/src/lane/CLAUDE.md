# rust/log/src/lane: the capsule's lanes: transport contract, client seam, platform bridge (capsule)

Bytes in, typed frames out. The capsule's three lockstep lanes (SOM0 management, SOA0 attach, SOSV supervisor) ride a
byte transport chosen once per platform; this folder holds the contracts the leg, the supervisor and every client
program against. Part of capsule; charter: rust/log/CLAUDE.md.

## Files
- `attach_proto/`: the attach and mgmt lanes' connection and role state machine (decides; the leg executes)
- `client.rs`: the dialing seam: `Client`, `PeerIdentity`, `PeerProcess`, `Endpoint`, `PlatformEndpoint`, `map_peer_auth_outcome` (the peer-authentication mapping both connects use)
- `mod.rs`: declares the lane modules; each file gates itself by platform
- `pipe_win/`: the Windows named-pipe transport, server and client
- `platform_transport.rs`: `PlatformTransport`, the capsule's `Transport` over `PlatformLaneServer`
- `socket_unix/`: the Unix domain-socket transport, server and client
- `test_progress.rs`: Unix-only server-local socket checkpoints; nonwaiting admission counts skipped records, and test/support snapshots retain bounded history.
- `transport.rs`: `Transport`, `TransportEvent`, `LaneServer`, `LaneEvent`, `TransportError`, the teardown bound and the servers' shared helpers
- `wire/`: the frame layouts of the three lanes, pure encode and decode

## Start here
`transport.rs` for the contract every lane server implements; `client.rs` for how a client dials one.

## Rules
- Every worker join is bounded: `join_within` polls `is_finished` against one deadline, and a teardown spends one `TEARDOWN_AGGREGATE_DEADLINE` across all its joins.
- `TransportError::is_endpoint_absent` is the one absence predicate on every platform.
- The platform is chosen once, by `client::PlatformEndpoint` and `transport::PlatformLaneServer`.
- A connection's outbound bytes are reserved in `OutboundBudget` before queueing and released when the write returns.
- `Endpoint::drop_spare` abandons work started for a supervisor attempt that ended before a voyage dial; local endpoints use its default no-op.
