# rust/log/src/identity: peer identity, the same-connection challenge (platform)

Before a reply on a local connection is trusted, the client proves the peer is this user's process, with a known pid and
creation time. The OS-specific steps 1-3 run first, in one file per platform; the shared wire steps 4-5 follow. Part of
platform; charter: rust/log/src/host/CLAUDE.md (written by a later unit, not yet present).

## Files
- `mod.rs`: declares the modules below.
- `challenge.rs`: the platform-neutral core: `ChallengeOutcome`, the connection trait, `exchange_identity`.
- `challenge_unix.rs`: Linux steps 1-3: `SO_PEERCRED` same-user check, pidfd pin, retained-pidfd process handle.
- `challenge_macos.rs`: macOS steps 1-3: one `LOCAL_PEERTOKEN` read (euid, pid and pidversion), `ChallengedProcess`, and `peer_euid_pid_created`, the daemon's accept-time read.
- `exit_watch_macos.rs`: the macOS kqueue `NOTE_EXIT` death watch, shared with `supervisor/probe/macos.rs`.
- `challenge_win.rs`: Windows steps 1-3: the pipe server's token SID and process handle.
- `exchange.rs`: the identity request and reply codec for the wire round trip (`feed`).
- `deadline.rs`: the three-state deadline race that bounds a blocking call (`run_with_deadline`).
- `os_account.rs`: this process's OS account as the OS issues it (`own_account_id`: `uid:<euid>` or the token's user SID), the string a hello declares.

## Start here
Read `challenge.rs` (`exchange_identity`, `ChallengeOutcome`) first, then the platform file's `challenge` and
`authenticate_server`.

## Rules
- The OS steps 1-3 (Linux `SO_PEERCRED` plus a pidfd pin, macOS one `LOCAL_PEERTOKEN` read, Windows the pipe server's
  token SID) run before the shared wire steps 4-5 in `challenge::exchange_identity`.
- The wire round trip is bounded by `deadline::run_with_deadline`.
- A reply that is not exactly one well-formed identity is `Foreign`, never `Proven` (`exchange.rs` `feed`).
- `created` is compared for equality only, in each OS's own unit: FILETIME bits, `/proc` start ticks, pidversion.
- The macOS kernel facts the challenge and the daemon's accept rest on (a client reading `LOCAL_PEERTOKEN` on its own
  fd sees the server's pid and a nonzero pidversion; a server reading its accepted fd sees the client's pid, a nonzero
  pidversion and the client's euid) are pinned by `rust/log/tests/macos_kernel_facts/`.
