# rust/log/src/identity/peer_owner: whose OS account holds the far end of an accepted loopback connection (platform)

An accepted loopback TCP connection is judged by the OS account that owns its far end: one lookup per connection, on a
blocking thread. Linux reads the kernel's TCP table; Windows reads the owner-module TCP table (the binding process and the bind time), refuses a process created after the
bind (a recycled pid), and then reads that process's token SID; macOS reads the kernel's TCP table (`net.inet.tcp.pcblist_n`) and judges the socket's creator; any other platform refuses. Part of platform; charter: rust/log/src/host/CLAUDE.md.

## Files
- `mod.rs`: the `PeerOwner` verdict, `tcp_peer_owner` (the per-platform lookup) and `serve_own` (the one TCP accept loop, which runs the owner check on each connection).
- `pcblist_n.rs`: the macOS kernel TCP table walker (`net.inet.tcp.pcblist_n`), compiled on macOS and for tests.

## Start here
Read `mod.rs`: `serve_own`, then `PeerOwner`, `tcp_peer_owner` and `admit`; `pcblist_n.rs` only for the macOS table.

## Rules
- Every TCP accept in Ship of Tools' Rust processes is `serve_own`'s (guarded by `rust/log/tests/single_accept.rs`); no caller can hand a loop a weaker check. Pluto's and `wglshow`'s servers are Julia children on ports of their own: no owner check reaches them, and their secret is the lock (it never reaches another account's command line, file or log).
- Only `PeerOwner::Mine` is served; every failure is `Unknown` and refuses (`admit`, private).
- A refused connection is closed with nothing read or written (`serve`); an accept error is retried after 50 ms.
- The macOS walker accepts only the kernel's six-record sequence per connection; any other length, order or truncation
  refuses (`pcblist_n.rs`).
- The first refusal per listener, port and owner is a warning; later ones are debug lines.
