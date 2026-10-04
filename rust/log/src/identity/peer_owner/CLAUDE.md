# rust/log/src/identity/peer_owner: whose OS account holds the far end of an accepted loopback connection (platform)

An accepted loopback TCP connection is judged by the OS account that owns its far end: one lookup per connection, on a
blocking thread. Linux reads the kernel's TCP table; macOS searches this account's own sockets for the peer within a
budget; any other platform refuses until its arm lands. Part of platform; charter: rust/log/src/host/CLAUDE.md
(written by a later unit, not yet present).

## Files
- `mod.rs`: the `PeerOwner` verdict, `tcp_peer_owner` (the per-platform lookup) and `admit` (the check a listener runs).

## Start here
Read `mod.rs`: `PeerOwner`, then `tcp_peer_owner` and `admit`.

## Rules
- Only `PeerOwner::Mine` is served; every failure is `Unknown` and refuses (`admit`).
- The first refusal per listener, port and owner is a warning; later ones are debug lines.
