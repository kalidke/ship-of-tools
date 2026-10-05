# rust/log/src/identity: peer identity, the same-connection challenge (platform)

Before a reply on a local connection is trusted, the client proves the peer is this user's process, with a known pid and
creation time. The OS-specific steps 1-3 run first, in one file per platform; the shared wire steps 4-5 follow. Part of
platform; charter: rust/log/src/host/CLAUDE.md.

## Files
- `mod.rs`: declares the modules below.
- `challenge.rs`: the platform-neutral core: `ChallengeOutcome`, the connection trait, `exchange_identity`.
- `challenge_unix.rs`: Linux steps 1-3: `SO_PEERCRED` same-user check, pidfd pin, retained-pidfd process handle.
- `challenge_macos.rs`: macOS steps 1-3: one `LOCAL_PEERTOKEN` read (pid and pidversion), `ChallengedProcess`.
- `exit_watch_macos.rs`: the macOS kqueue `NOTE_EXIT` death watch, shared with `supervisor/probe/macos.rs`.
- `challenge_win.rs`: Windows steps 1-3: the pipe server's token SID and process handle.
- `connect_own.rs`: the one rule for a local endpoint reached by name: `own_socket`, `own_pipe`, `connect_own`.
- `impersonation_probe.rs`: test support (Windows, `test-support` feature): the impersonation level a pipe's server gets over a client's handle.
- `exchange.rs`: the identity request and reply codec for the wire round trip (`feed`).
- `deadline.rs`: the three-state deadline race that bounds a blocking call (`run_with_deadline`).
- `os_account.rs`: this process's OS account as the OS issues it (`own_account_id`), the value two accounts on one box are told apart by.
- `peer_owner/`: whose OS account owns the far end of an accepted loopback connection.

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
- No Rust code dials a local socket or pipe by name outside `connect_own` and the sites that apply its rule, except through a connector the list in `rust/clippy.toml` names without an `#[allow]` and its reason: rust.yml's "Disallowed methods" step (`disallowed_methods`) fails on one. It cannot see `std::fs::OpenOptions::open` of a pipe path; `rust/log/tests/connect_own.rs` only checks that `net/transport/mod.rs` and `topology/dial.rs` contain the rule's names (`own_socket(`, `connect_own(` or `own_pipe(`), not their order and not any other file's opener.
- A client that reaches this box's daemon or a relay socket by name speaks to it only after `connect_own`'s rule passes: on Unix before the connect, on Windows before the first byte. The Unix rule checks the socket's own folder only, not the folders above it (a custom runtime path under another account's writable, non-sticky folder is not covered; the daemon's bind check has that limit too, and checks the folders from the runtime folder down for a derived path); every Rust client of a Windows daemon pipe opens it at identification level (`SECURITY_IDENTIFICATION`): `connect_own`, `topology/dial.rs` and the daemon's own start probe (`socket_answers`).
