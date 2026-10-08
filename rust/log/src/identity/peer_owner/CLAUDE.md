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
- TCP accepts use `serve_own` and its private owner decision; the existing accept lint remains. Native owner tests prove refusal before handler entry and a same-account serving control; no listener-spelling catalog supplies that proof.
- Only `PeerOwner::Mine` is served; every failure is `Unknown` and refuses (`admit`, private).
- A refused connection is closed with nothing read or written (`serve`). On Windows the accepted socket is made
  non-inheritable the moment it is accepted, before the check, so a child process started after that cannot keep it
  open. An accept error is retried after 50 ms.
- One listener runs at most `MAX_LOOKUPS` owner lookups at once and takes its turn before the accept, so a flood waits
  in the kernel's backlog and cannot fill the process's blocking pool or its descriptors.
- The macOS walker accepts only the kernel's six-record sequence per connection; any other length, order or
  truncation refuses (`pcblist_n.rs`).
- The first refusal per listener, port and owner is a warning; later ones are debug lines.
