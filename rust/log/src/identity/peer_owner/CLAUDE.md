# rust/log/src/identity/peer_owner: whose OS account holds the far end of an accepted loopback connection (platform)

An accepted loopback TCP connection is judged by the OS account that owns its far end: one lookup per connection, on a
blocking thread. Linux reads the kernel's TCP table; Windows reads the owner-pid TCP table and then that process's
token SID; macOS reads the kernel's TCP table (`net.inet.tcp.pcblist_n`) and judges the socket's creator; any other platform refuses. Part of platform; charter: rust/log/src/host/CLAUDE.md
(written by a later unit, not yet present).

## Files
- `mod.rs`: the `PeerOwner` verdict, `tcp_peer_owner` (the per-platform lookup) and `admit` (the check a listener runs).
- `pcblist_n.rs`: the macOS kernel TCP table walker (`net.inet.tcp.pcblist_n`), compiled on macOS and for tests.

## Start here
Read `mod.rs`: `PeerOwner`, then `tcp_peer_owner` and `admit`; `pcblist_n.rs` only for the macOS table.

## Rules
- Only `PeerOwner::Mine` is served; every failure is `Unknown` and refuses (`admit`).
- The macOS walker accepts only the kernel's six-record sequence per connection; any other length, order or truncation
  refuses (`pcblist_n.rs`).
- The first refusal per listener, port and owner is a warning; later ones are debug lines.
