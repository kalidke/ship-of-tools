# rust/log/src/identity/peer_owner: whose OS account holds the far end of an accepted loopback connection (platform)

An accepted loopback TCP connection is judged by the OS account that owns its far end: one lookup per connection, on a
blocking thread, in the kernel's TCP table. Part of platform; charter: rust/log/src/host/CLAUDE.md (written by a later
unit, not yet present). The Linux lookup is built; the Windows and macOS arms follow.

## Files
- `mod.rs`: the `PeerOwner` verdict and the Linux lookup of a connection's owner in `/proc/net/tcp{,6}`.

## Start here
Read `mod.rs`: `PeerOwner`, then the table parser and its `verdict`.

## Rules
- Only `PeerOwner::Mine` is served; every failure is `Unknown` and refuses (`verdict`).
