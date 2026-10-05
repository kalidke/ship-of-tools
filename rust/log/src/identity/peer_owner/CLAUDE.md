# rust/log/src/identity/peer_owner: whose OS account holds the far end of an accepted loopback connection (platform)

An accepted loopback TCP connection is judged by the OS account that owns its far end: one lookup per connection, on a
blocking thread. Linux reads the kernel's TCP table; Windows reads the owner-module TCP table (the binding process and
the bind time), refuses a process created after the bind (a recycled pid), and then reads that process's token SID;
macOS reads the kernel's TCP table (`net.inet.tcp.pcblist_n`) and judges the socket's creator; any other platform
refuses. Part of platform; charter: rust/log/src/host/CLAUDE.md.

## Files
- `mod.rs`: the `PeerOwner` verdict, `tcp_peer_owner` (the per-platform lookup) and `serve_own` (the one TCP accept
  loop, which runs the owner check on each connection).
- `pcblist_n.rs`: the macOS kernel TCP table walker (`net.inet.tcp.pcblist_n`), compiled on macOS and for tests.

## Start here
Read `mod.rs`: `serve_own`, then `PeerOwner`, `tcp_peer_owner` and `admit`; `pcblist_n.rs` only for the macOS table.

## Rules
- Every TCP accept of the Rust processes is `serve_own`'s: `rust/clippy.toml` disallows every accept, and the one
  TCP accept carries the allow named `listener: page (TCP)` in `serve`; `rust/log/tests/isolation_guards.rs` lists
  every listener's allow. No caller can hand a loop a weaker check.
- Only `PeerOwner::Mine` is served; every failure is `Unknown` and refuses (`admit`, private).
- A refused connection is closed with nothing read or written (`serve`); an accept error is retried after 50 ms.
- One listener runs at most `MAX_LOOKUPS` owner lookups at once and takes its turn before the accept, so a flood waits
  in the kernel's backlog and cannot fill the process's blocking pool or its descriptors.
- The macOS walker accepts only the kernel's six-record sequence per connection; any other length, order or
  truncation refuses (`pcblist_n.rs`).
- The first refusal per listener, port and owner is a warning; later ones are debug lines.
