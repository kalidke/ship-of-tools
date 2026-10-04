# rust/log/src/identity: peer identity, the same-connection challenge (platform)

Before a reply on a local connection is trusted, the client proves the peer is this user's process, with a known pid and
creation time. The OS-specific steps 1-3 run first, in one file per platform; the shared wire steps 4-5 follow. Part of
platform; charter: rust/log/src/host/CLAUDE.md (written by a later unit, not yet present).

## Files
- `mod.rs`: declares the modules below.
- `challenge.rs`: the platform-neutral core: `ChallengeOutcome`, the connection trait, `exchange_identity`.
- `challenge_unix.rs`: Linux steps 1-3: `SO_PEERCRED` same-user check, pidfd pin, retained-pidfd process handle.
- `challenge_macos.rs`: macOS steps 1-3: one `LOCAL_PEERTOKEN` read (pid and pidversion), `ChallengedProcess`.
- `exit_watch_macos.rs`: the macOS kqueue `NOTE_EXIT` death watch, shared with `supervisor/probe/macos.rs`.
- `challenge_win.rs`: Windows steps 1-3: the pipe server's token SID and process handle.
- `connect_own.rs`: the one rule for a local endpoint reached by name: `own_socket`, `own_pipe`, `connect_own`.
- `exchange.rs`: the identity request and reply codec for the wire round trip (`feed`).
- `deadline.rs`: the three-state deadline race that bounds a blocking call (`run_with_deadline`).

## Start here
Read `challenge.rs` (`exchange_identity`, `ChallengeOutcome`) first, then the platform file's `challenge` and
`authenticate_server`.

## Rules
- The OS steps 1-3 (Linux `SO_PEERCRED` plus a pidfd pin, macOS one `LOCAL_PEERTOKEN` read, Windows the pipe server's
  token SID) run before the shared wire steps 4-5 in `challenge::exchange_identity`.
- The wire round trip is bounded by `deadline::run_with_deadline`.
- A reply that is not exactly one well-formed identity is `Foreign`, never `Proven` (`exchange.rs` `feed`).
- `created` is compared for equality only, in each OS's own unit: FILETIME bits, `/proc` start ticks, pidversion.
- The macOS kernel fact the challenge rests on (a client reading `LOCAL_PEERTOKEN` on its own fd sees the server's pid
  and a nonzero pidversion) is pinned by `rust/log/tests/macos_kernel_facts/`.
- A client that reaches this box's daemon or a relay socket by name speaks to it only after `connect_own`'s rule passes: on Unix before the connect, on Windows before the first byte.
