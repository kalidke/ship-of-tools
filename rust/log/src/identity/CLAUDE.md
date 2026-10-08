# rust/log/src/identity: peer identity, the same-connection challenge (platform)

Before a reply on a local connection is trusted, the client checks the kernel's recorded account for the connection
and observes a pid and creation time. On Linux and macOS, steps 1-3 authenticate the account the kernel recorded for the
connection, not the descriptor
holder's current euid; on macOS the pid is the live token's observed process. Credential transitions and descriptor
transfers leave that account record unchanged. Process attribution requires an honest
responder reporting its own identity; liveness after registration also requires it to read the request before replying.
The OS-specific steps 1-3 run first, in one file per platform; the shared wire steps 4-5 follow. Part of
platform; charter: rust/log/src/host/CLAUDE.md.

## Files
- `mod.rs`: declares the modules below.
- `challenge.rs`: the platform-neutral core: `ChallengeOutcome`, the connection trait, `exchange_identity`.
- `challenge_unix.rs`: Linux steps 1-3: `SO_PEERCRED` same-user check, pidfd pin, retained-pidfd process handle.
- `challenge_macos.rs`: macOS steps 1-3 and `peer_euid_pid_created`, the one macOS reader of a connected peer (its euid from `getpeereid`, its pid and pidversion from one `LOCAL_PEERTOKEN` read), which the daemon's accept-time admission also calls; `ChallengedProcess`.
- `exit_watch_macos.rs`: the macOS kqueue `NOTE_EXIT` death watch, shared with `supervisor/probe/macos.rs`.
- `challenge_win.rs`: Windows steps 1-3: the pipe server's token SID and process handle.
- `connect_own.rs`: the one rule for a local endpoint reached by name: `own_pipe` (Windows) and `connect_own`, which
  checks on Unix the account that listens on the connected socket.
- `impersonation_probe.rs`: test support (Windows, `test-support` feature): the impersonation level a pipe's server gets over a client's handle.
- `exchange.rs`: the identity request and reply codec for the wire round trip (`feed`).
- `deadline.rs`: the three-state deadline race that bounds a blocking call (`run_with_deadline`).
- `os_account.rs`: this process's OS account as the OS issues it (`own_account_id`: `uid:<euid>` or the token's user SID), the string a hello declares.
- `peer_owner/`: whose OS account owns the far end of an accepted loopback connection.

## Start here
Read `challenge.rs` (`exchange_identity`, `ChallengeOutcome`) first, then the platform file's `challenge` and
`authenticate_server`.

## Rules
- The OS steps 1-3 (Linux `SO_PEERCRED` plus a pidfd pin, macOS `getpeereid` plus one `LOCAL_PEERTOKEN` read,
  Windows the pipe server's token SID) run before the shared wire steps 4-5 in `challenge::exchange_identity`.
- A peer's account is the kernel's record of the connection, never the wire's or a live lookup's: Linux `SO_PEERCRED`, macOS `getpeereid` (for a client, the listener's credentials at `listen()`; for a server, the client's at `connect(2)`). macOS `LOCAL_PEERTOKEN` looks the peer's pid up when it is read, so it observes a process (pid and pidversion), never the account. The identity request carries no nonce: process attribution requires an honest responder reporting its own identity, and liveness after registration also requires it to read the request before replying. Cached credentials authorize connection provenance, not the descriptor holder's current euid; credential transitions or descriptor transfers do not change that record. The source pin checks the production `LOCAL_PEERTOKEN`/`TOK_EUID` spellings and whitespace-normalized `.val` accesses in the private `AuditToken` module, permitting only pid/pidversion reads with indices pinned to 5/7; it does not analyze arbitrary equivalent Rust or numeric socket-option calls, and excludes test-only reads.
- The wire round trip is bounded by `deadline::run_with_deadline`, whose early settlement on a panic is disarmed the
  moment the body returns: it settles exactly the runs whose body did not return, whatever the thread was doing before
  (a run made during a panic's unwind, a panic caught inside such a cleanup).
- A reply that is not exactly one well-formed identity is `Foreign`, never `Proven` (`exchange.rs` `feed`).
- `created` is compared for equality only, in each OS's own unit: FILETIME bits, `/proc` start ticks, pidversion.
- The macOS kernel facts the challenge and the daemon's accept rest on (a client reading `LOCAL_PEERTOKEN` on its own
  fd sees the server's pid and a nonzero pidversion; a server reading its accepted fd sees the client's pid and a nonzero
  pidversion) are pinned by `rust/log/tests/macos_kernel_facts/`; that `getpeereid` keeps the credentials cached at `listen()`
  and `connect(2)` after the peer changes its euid, by the two credential-transition tests in `rust/log/tests/challenge_macos.rs`.
- No Rust code dials a local socket or pipe by name outside `connect_own` and the sites that apply its rule, except
  through a connector the list in `rust/clippy.toml` names without an `#[allow]` and its reason: rust.yml's "Disallowed
  methods" step (`disallowed_methods`) fails on one. It cannot see `std::fs::OpenOptions::open` of a pipe path; the
  behavior tests in `rust/backend/src/topology/dial.rs` and `rust/frontend/src/net/transport/tests.rs` exercise the
  production topology `connect` and window `connect_pipe` entries and require each Unix and Windows arm to refuse
  another account's endpoint, with an additional zero-received-byte observation in the Unix fixtures; this covers those
  exercised entries, while a new pipe-path opener and its account check before client I/O remain questions for review.
- A client that reaches this box's daemon or a relay socket by name speaks to it only after `connect_own`'s rule passes,
  after the connect and before the first byte. The connector uses a fixed `CONNECT_BOUND` retry budget. An attempt or
  wait already in progress finishes first: Unix includes a 20 ms retry sleep, Windows a 200 ms named-pipe wait. This is
  not an exact elapsed-time limit. On Unix the rule reads the account the kernel recorded when the socket's listener
  called `listen()` (`SO_PEERCRED` on Linux, `getpeereid` on macOS): the account of the process that called `listen()`,
  so a listener this account hands to another process by `SCM_RIGHTS` stays this account's. A process in a user
  namespace that maps no uid of its own reads its own uid and every listener's as the overflow uid
  (`/proc/sys/kernel/overflowuid`), so on Linux `connect_own` refuses a listener at that uid when its own uid is
  unmapped in `/proc/self/uid_map` (`overflow_uid_is_ambiguous`). The daemon's bind check is by pathname and covers the
  folders from the runtime folder down for a derived path, not the folders above. Every Rust client of a Windows daemon
  pipe opens it at identification level (`SECURITY_IDENTIFICATION`): `connect_own`, `topology/dial.rs` and the daemon's
  own start probe (`socket_answers`).
