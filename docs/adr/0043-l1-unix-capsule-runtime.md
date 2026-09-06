# ADR 0043: L1-unix — the capsule runtime on Unix hosts

**Status:** Proposed (2026-09-06). Design pass for the lane series that makes
ADR 0042's rule — "the capsule is the default runtime for every NEW session on
EVERY host" — true on the Linux backend hosts, where today every row is still
a tmux row and no session leaves a Ship's Log record. Builds on ADR 0037 (P1:
the Linux producer and the record format; P5: the tmux path retires by
attrition), ADR 0039 (the format), ADR 0041 (the Windows runtime this ports),
and ADR 0042 (L1: the capsule workspace runtime).

## The decision in one paragraph

Port by **property**, not by mechanism. `pipe_win.rs` and `challenge.rs` pin
twenty-eight properties (listed below) with Win32 mechanisms — overlapped I/O,
`CancelIoEx`, named-pipe instances, SIDs, process handles. The Unix runtime
satisfies each property with the ordinary POSIX primitive that provides it —
`AF_UNIX` sockets, `shutdown(2)`, `poll(2)`, `SO_PEERCRED`, `pidfd` — and
**deletes** the machinery that served invariants Unix does not have. Everything
the two platforms share (the contract, the bounds, the pure helpers, the
identity vocabulary, the classifier) is hoisted into neutral modules FIRST as
pure moves, so Windows CI proves no behaviour change before a line of Unix code
exists. `sot-log` gains no new dependency: `libc` is already there for Unix.

## Lanes

Each lane is one PR: a design pass here, a brief, a Sonnet implementation, a
manager review, one Codex round, CI green, merge (the standing rule).

- **LU0 — DONE (#212).** `TransportEvent`, `Transport`, the shared 20 s
  teardown bound and `join_within` in the ungated `transport.rs`.
- **LU1a — the neutral vocabulary (pure moves + one extraction).**
  `challenge.rs` keeps only what has no OS in it: `ChallengeOutcome<P>`, a
  three-method `ChallengeableConnection` (`write_all`/`read`/`cancel`), and
  `exchange_identity(conn, exchange, deadline)` — steps 4–5 of the challenge
  (the deadline-bounded request/reply loop, extracted verbatim from today's
  `challenge()`), so both platforms run the identical wire half. Windows steps
  1–3, `ChallengedProcess`, `authenticate_server`, the `HANDLE` helpers and a
  `PipeChallengeable: ChallengeableConnection { fn raw_handle }` extension trait
  move to `challenge_win.rs`. `probe.rs` keeps the neutral enums
  (`ConnectOutcome`, `SpawnOutcome`, `WaitOutcome`, `FenceProbe`), the
  `ProbeOps` trait and the scripted test support; `RealProbeOps`/`SpawnedChild`
  move to `probe_win.rs`. `classify.rs` loses its `#![cfg(windows)]` — it makes
  no OS call — and its unit tests run on the Linux leg for the first time. The
  pure transport helpers `OutboundBudget`, `StartGate` and the five bound
  constants (`READ_BUF_LEN`, `OUTBOUND_BUDGET_BYTES`, `EVENTS_CHANNEL_CAP`,
  `EVENTS_RETRY_INTERVAL`, `BYTES_ABANDON_AFTER`) move into `transport.rs`.
- **LU1b — `socket_unix.rs` + `socket_transport.rs` + `tests/socket_unix.rs`.**
  `SocketServer` (bind/accept, per-connection reader+writer threads, one bounded
  events channel, byte-budgeted outbound, two-phase teardown), `SocketClient`
  (`write_all`/`read`/`cancel`), `connect_voyage_socket`, `SocketError`, and
  the bridge to `Transport`. `Error::Socket(#[from])` under `cfg(unix)`.
- **LU1c — `challenge_unix.rs` + `tests/challenge_unix.rs`** (Linux).
- **LU2 — one producer.** A `Producer` trait over ConPTY and `openpty`;
  `capsule.rs::run` gains the `commands` receiver and the `Transport`; the
  Windows `run` loop becomes the one loop; `capsule.rs` stops being a second
  source of truth. A `sot-pty-helper` test producer mirrors
  `sot-conpty-helper`; `e2e_socket.rs` mirrors `e2e_pipe.rs`. The parent-death
  lease becomes an inherited pipe whose EOF is the death signal.
- **LU3 — supervisor, `sot-capsule`, `supervisor_client`, `fe_client` on
  Unix.** The consumers go generic over a client trait (today every one names
  `PipeClient`); `PipeError`/`SocketError` unify into one `TransportError`;
  the SID-flavoured names (`SidAuthOutcome`, `SidAuthenticated`) become
  peer-flavoured. `sot-capsule`'s three `main`s collapse to two.
- **LU4 — the daemon.** `mod unix_runtime` beside `windows_runtime` in
  `capsule_workspace.rs` (`setsid` detach, `resume_all`); the `cfg(windows)`
  gates in `server.rs`/`handlers.rs`/`workspaces.rs` open; the
  `capsule_workspaces` test runs on Linux; a budgeted `ubuntu` CI job mirrors
  `conpty-windows-2022`. From then on `workspace.create` on a Linux host makes
  a capsule row, and ADR 0042's rule is true everywhere.

## The properties (what `pipe_win`/`challenge` pin; what LU1 must satisfy)

Transport — endpoint: (1) only this user may open it, enforced at creation;
(2) machine-local; (3) a rival cannot silently take the name — a squat is a
loud bind failure; (4) the name is held continuously while accepting, across
churn; (5) the name disappears synchronously, before any blocking join.
Connections: (6) `ConnId` is a sequential `u64`, never reused, one id space
shared with `attach_proto`; (7) a hard, loudly validated ceiling on
simultaneous connections. Events: (8) lifecycle events are never dropped;
(9) only `Bytes` may be abandoned, and abandoning forces a guaranteed
`Closed`; (10) inbound is bounded by one channel with no buffer in front of
it; (11) outbound is bounded in bytes per connection including the in-flight
item, and `send` never blocks. I/O: (12) a blocking read or write is
cancellable from another thread, promptly, including a write stalled on peer
backpressure; (13) `read` returning 0 is ordered EOF, never a spurious empty
completion; (14) workers do not touch a connection until it is registered and
its "opened" event is queued. Teardown: (15) one owner joins, teardown is
at-most-once per connection; (16) two independently observable phases —
`disconnect_listener` never blocks, `join_workers(deadline)` shares one
externally supplied absolute deadline and returns `false` loudly, which is
terminal; (17) both phases idempotent, `Drop` a standalone safety net with a
fresh 20 s budget; (18) connect is bounded (2 s) and retries only the
"busy / not yet there" family. Identity: (19) the peer's identity comes from
the kernel's view of the connection, never from the peer's bytes; (20) same-
user equality is checked before a single peer byte is trusted; (21) a proof
retains something that pins the kernel process object against pid reuse;
(22) the reply is bound to the OS observation, pid compared before creation
time, so a wrong pid is always `Foreign`, never `Undetermined`; (23) three
outcomes, never two — `Proven` / `Foreign` (well-formed wrong, never retried)
/ `Undetermined` (any OS failure, EOF or timeout); (24) steps 4–5 run under
the three-state deadline race in `deadline.rs`, unchanged; (25) same-user-only
authentication and the full five-step proof are different types. Surface and
tests: (26) the client surface is exactly `write_all`/`read`/`cancel`;
(27) real-process tests run one per isolated child process, bounded;
(28) a `#![cfg(unix)]` test file needs no new CI machinery.

## Decisions for LU1b and LU1c

1. **Socket paths live in the per-user runtime dir, named by id.**
   `<runtime_sot_dir>/voyage-<uuid>.sock` and
   `<runtime_sot_dir>/supervisor-<h>.sock` — the same two name families as the
   pipes, the same `h` (`state_dir_hash`). The derivation must be one pure
   function of (uid, id) that every process computes identically, and the
   daemon's session sockets already have exactly that in
   `sot-protocol::session_socket` (`runtime_sot_dir`, `is_private_dir`,
   `current_uid`). It moves DOWN into `sot-log`'s `state_dir.rs` — the module
   that already answers "where does this host keep sot's files" — and
   `sot-protocol` re-exports it; the frontend and the backend already depend on
   both crates, the updater on neither, so no binary grows. `sun_path` is 108
   bytes on Linux: the derivation checks the length and fails loudly
   (`PathTooLong`) rather than truncating.
2. **Squat and stale-file rule.** `bind` → `EADDRINUSE` → a connect probe: a
   live listener means `AlreadyBound` (loud, property 3); `ECONNREFUSED` means
   a stale file, which the binder unlinks and retries once. Only the bind path
   ever unlinks, and only after the probe. `disconnect_listener` unlinks its
   own path first thing (property 5); the inode the listener holds keeps the
   name across churn (property 4).
3. **Permissions.** The socket dir is owner-only (0700, owner-checked with
   `is_private_dir`, never a symlink); the socket is created under `umask
   0077`, then `chmod 0600`, then verified (property 1). `AF_UNIX` is asserted
   (property 2).
4. **Ceiling.** An explicit `max_connections` (1..=255, `InvalidMaxConnections`,
   property 7). Unix cannot refuse at connect time — the kernel completes the
   handshake from the backlog — so at capacity the acceptor accepts and closes
   immediately, and the connect bound treats EOF-before-first-byte as the
   "busy" family alongside `ECONNREFUSED` and `ENOENT` (property 18). A
   deliberate, documented difference from `PIPE_BUSY`; the classifier already
   retries `Undetermined`.
5. **Cancellation is `shutdown(2)`.** A blocked `read` returns 0, a blocked
   `write` gets `EPIPE`; a `cancelled` flag set before the shutdown turns those
   into `Cancelled` (property 12). The acceptor blocks in `poll(2)` over the
   listener and a wake pipe, so `disconnect_listener` wakes it without a
   connect-to-self trick. **Nothing from `CompletionUnproven` is ported** — no
   `mem::forget`, no `process::abort` — because POSIX `read`/`write` never
   borrow the caller's buffer past the call; that apparatus served a Windows
   invariant only.
6. **Bounds are the same numbers on both platforms**, by construction: the
   constants are hoisted in LU1a and imported by both transports.
7. **Client contract parity.** One slot per direction, so a second concurrent
   same-direction call is `ConcurrentSubmit` before touching the OS, exactly as
   today (the consumers may rely on it; dropping it is LU3's call together
   with the error unification). `write_all` loops partial writes to keep the
   all-or-nothing contract.
8. **Challenge on Linux (LU1c).** `SO_PEERCRED` returns `(pid, uid, gid)` in one
   call and collapses steps 1–3: `uid == geteuid()` is the same-user check
   (property 20). The process object is pinned with `pidfd_open` (property 21);
   where a pidfd is unavailable the fallback is `/proc/<pid>/stat` start time
   re-verified before every signal. `created` is that start time (the wire
   only compares it for equality); `wait` is `poll` on the pidfd; `terminate` is
   `pidfd_send_signal(SIGKILL)`. **One honest gap:** Linux exposes an exit
   status only for a process's own children, so
   `exit_code_after_confirmed_exit` for an ADOPTED supervisor is not available
   here — the Unix type returns "exited, status unknown", and LU3 must make the
   supervisor's adopted-exit classification tolerate it. `challenge_unix.rs`
   is `cfg(target_os = "linux")`; other Unix fails closed with
   `Error::Unsupported` (macOS has no pid in `getpeereid`; it stays
   experimental).
9. **Error types.** `SocketError` is a sibling of `PipeError` for now, with
   the same variant vocabulary minus the Windows-only ones plus `PathTooLong`
   and `AlreadyBound`; one `TransportError` comes with the generic clients in
   LU3, not before.
10. **Tests.** `tests/socket_unix.rs` (`#![cfg(unix)]`) copies the
    `run_isolated` harness verbatim with `SOCKET_UNIX_TEST_CHILD`, ports the
    twenty portable pipe tests by type swap, and replaces the Windows-only
    assertions with their analogues: socket mode 0600 in a 0700 owner-checked
    dir for the descriptor test; `EADDRINUSE` throughout and success only after
    unlink for the squat test; rival bind succeeds after `disconnect_listener`
    for the name-freeing test; `/proc/self/fd` count for the handle-leak test;
    `SO_PEERCRED.pid` of a real child listener for the cross-process test. The
    blanket `ubuntu-latest` job runs them; the budgeted Linux job waits for LU4.

## What this deletes

On Unix: the completion-proof apparatus, instance recycling, the SDDL builder,
the `WaitNamedPipe` retry. In the crate: `classify.rs`'s gate, `capsule.rs` as
a second producer loop (LU2), the drawer-era SID names (LU3), the third
`main` in `sot-capsule` (LU3).

## Open for the maintainer

1. macOS: fail closed at the challenge (no capsules on macOS) until someone
   needs them — acceptable?
2. Capacity semantics (decision 4): accept-then-close plus a retrying client,
   instead of a kernel-level refusal — acceptable?
3. The dependency edge `sot-protocol → sot-log` (decision 1) — acceptable, or
   should the derivation be duplicated with a cross-crate equality test instead?
