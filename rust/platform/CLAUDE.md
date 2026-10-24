# rust/platform: platform (contract)

The bottom of the workspace: one answer per machine fact and one primitive per OS mechanism, for every crate above it:
the local connection (a bounded client of a socket or pipe on this box, reaching only this account) and the masking of
page secrets in logs among them.
The crate (`sot-platform`) depends on nothing of ours; sot-capsule (the capsule), sot-protocol (wire), the backend, the
window and the updater depend on it, and nothing re-exports it. It holds no runtime state and starts no process for
anyone but the native launcher it hands out.

## Guarantees
Each states what the rest may rely on, the mechanism it rests on, and the test that holds it (the Tests section lists
where each lives).

1. **A machine fact has one answer.** The state folder (`sot_state_dir`), the config folder (`sot_config_dir`), the
   runtime base (`runtime_sot_dir`), the host name (`host_name`, `SOT_SELF_HOST` over the kernel's name), the OS
   account (`own_account_id`) and the folder hash lane sockets are named by (`state_dir_hash`) are each defined once,
   here, from an `Env` value the caller supplies or the process's own; a second rule anywhere is a defect. Raw host
   name readers that remain, each for a format that must not change under installed releases: the comm handle's host
   component (`rust/backend/src/comm/identity/context.rs`), the updater's stage lock record (`rust/updater/src/lock.rs`)
   and the window's resume file name (`rust/frontend/src/ui/persist/resume.rs`); they fold with their owners' passes.
   `host_name` returns `Err`, never a guessed name. A runtime base a process binds and another dials agrees only
   while the environment does not change; the supported setups keep it fixed for a boot (Linux: linger and the user
   manager's `XDG_RUNTIME_DIR`; macOS: none; Windows: no runtime base). A boot without logind binds at
   `/tmp/sot-<uid>`.
2. **A publication is durable and never clobbers.** `publish_noreplace` renames without replacing, flushes the
   renamed file (Windows) and then the parent; the caller flushes the source first (`write_durable` does); `write_durable` (tmp, fsync, replace,
   directory sync) and `remove_durable` are the one write and delete for a record a later start acts on; on Windows a
   removal has no parent sync (Win32 has no write-through delete). Rests on
   renameat2/renamex_np/MoveFileEx write-through; only linux, macos and windows build.
3. **A lock is the kernel's.** A fence, the daemon's single-instance lock and a held folder's lock are released at
   once by a dropped guard (`WriterLock`'s Drop unlocks) and when the holder dies and the last descriptor of the locked
   file closes; a forked child that kept a copy holds it. The descriptors this crate opens are close-on-exec, so no
   exec'd child inherits one; a `HandoverLock` only closes on drop, so a copy in a child keeps the fence held, by design. A contended lock fails within
   `RETRY_DEADLINE_MS` as "lock held". Kernel file locks are taken only in these guards and `InboxLock`
   (messaging's, in `rust/backend/src/comm/mail/inbox.rs`); `rust/clippy.toml` refuses them elsewhere.
4. **A held folder stays while its maker lives.** (Its lifetime is the `HeldDir` guard's or the maker process's,
   whichever the lock outlasts; on Windows the folder's parent is assumed private to the account.) `remove_ended` removes a folder only once it can take the folder's
   lock, which `HeldDir::create` takes under a staging name before renaming the lock file to the name a removal
   looks for; another computer sharing the folder, another pid namespace or a reused pid never removes a live
   maker's folder. A link, a file, another account's folder and a folder whose lock cannot be opened stay. Across
   computers this needs locks every client sees (NFS with its lock service), and only while the maker's client
   holds its lease. A temporary folder on a file system that refuses locks stops `HeldDir::create`.
5. **A descriptor is born close-on-exec.** A socket, pipe or pty made through `socket_cloexec`, `pipe_cloexec` or
   `openpty_cloexec` is never, between its birth and its flag, in a child forked through a guarded spawn (Linux in the
   creating call; macOS under a process-wide birth lock every fork site takes around its fork, `fork_guard`);
   `inherit_none` is the belt for descriptors made elsewhere (std, tokio, mio). Windows hardens a process's own stdio
   handles (`harden_own_stdio`) and makes owner-only descriptors at birth (`owner_protected_pipe_descriptor`).
6. **A local reply is trusted only after the challenge.** The OS steps (the peer's account and pid as the OS
   recorded them, then equality of identity) precede the wire steps (`exchange_identity`, over an
   `IdentityExchange` the lane provides); `run_with_deadline` bounds the wire steps, and the OS steps read kernel
   records without waiting on the peer. Two outcomes differ: the full challenge earns `Proven` (OS identity and the
   wire's pid and creation time agree), while `authenticate_server` proves the account alone (`PeerAuthenticated`).
   A wrong, partial or late reply is `Foreign` or `Undetermined`; a second complete reply inside the same read is
   `Foreign`, and bytes after a conclusive answer are the exchange's to refuse (the lanes' do). On macOS the peer is
   attributed by `LOCAL_PEERPID` and its start time; the assumptions are stated in `challenge_macos.rs`'s module doc. A TCP connection on loopback is admitted only when the OS names this account as its owner
   (`peer_owner::serve_own`). Rests on SO_PEERCRED/pidfd, LOCAL_PEERPID/kqueue, the pipe's SID and impersonation
   level, the TCP owner tables.
7. **Storage exhaustion is known by its native code.** ENOSPC and EDQUOT; on Windows ERROR_DISK_FULL,
   ERROR_HANDLE_DISK_FULL and ERROR_DISK_QUOTA_EXCEEDED, read from the `io::Error` an `Error::Io` carries, never from
   text and never from a transport error (`native_storage_code`; the capsule reads its own error type through it). `preflight_volume` and Windows `io_ctx` return such
   an error as itself.
8. **A volume is proved before a store opens on it.** `preflight_volume` refuses a root whose filesystem cannot do
   the operations the store uses (create, no-clobber rename, fsync), a network filesystem above all (NFS answers EINVAL
   to every `renameat2` flag), so a state root on a network home points `XDG_STATE_HOME` at local disk. It does not
   prove persistent backing: rows refuses volatile storage itself (`rows/spawn/state_root.rs`). The Windows
   preflight has no test.
9. **A restart is paced.** A connection or child restarted through `Redial` waits from its caller's floor,
   doubling to its cap, and starts over only after one that lasted `STABLE` (60 s) or at a person's request; an
   answered hello, a bare connect or a generation alone does not restart it.
10. **A directory pin opens only a directory (Unix).** `dir_identity` opens with `O_DIRECTORY`, so a FIFO or any
    other non-directory fails at once and never waits. On Windows it opens with backup semantics and accepts an
    ordinary file (gap, listed in the result file).
11. **Private means private (Unix).** `is_private_dir` and `secure_private_dir` accept only an owner-only directory
    that is not a link; `secure_socket_dir` secures each level under the runtime base the same way. `ensure_private_dir`
    is the weaker form: it creates the folder, follows a symlink and repairs the mode to 0700. On Windows
    `is_private_dir` is only `is_dir`; the account's profile folders are the protection. A refusal from the OS keeps
    the OS error as its source (`Error::Path`).
12. **A job ends what is in it when its last handle closes (Windows).** `host::winjob::AnonymousJob` is an anonymous,
    non-inheritable job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` alone and no breakaway permitted, so every process
    started inside it stays inside and the kernel ends them all when the last handle closes, however the holder died.
    Assigning a process, sequencing a termination and resuming a suspended start are the caller's (containment's and the
    capsule's ConPTY agent). Limits: a process started through a broker (WMI, COM activation, a scheduled task, a service)
    is outside the job.

13. **A local connection is bounded, cancellable and reaches only this account.** `transport::connect_own` connects
    to a socket or pipe by path and then proves, before the caller writes, that this OS account serves it (the
    listener's account as the kernel recorded it at `listen()`, or the pipe server's SID); a different account's
    listener is refused. The connect is bounded by `CONNECT_BOUND` (2 s): a busy endpoint (a full backlog, a claimed
    pipe instance, an interrupted attempt) is retried within it (macOS may answer a full backlog with an immediate
    `ConnectionRefused` instead, which reads as absent), an absent one (no listener, no pipe) is refused at the
    first attempt and never charged to the bound. A `Client` (`SocketClient`, `PipeClient`) reads and writes blocking
    but is cancelled from any thread: `cancel` ends a blocked read or write at once (Unix by `shutdown(2)`, Windows by
    `CancelIoEx`), and a cancelled client refuses every later call; a second same-direction call is refused as
    `ConcurrentSubmit` before it touches the OS; `Ok(0)` is ordered EOF, never a spurious completion. On Unix a terminal write
    failure latches the client like a cancel. `accept_policy` is the one rule an accept loop applies to an accept
    error: retry, sleep `ACCEPT_SLEEP` on resource exhaustion, or end. Rests on `shutdown(2)`/`CancelIoEx`, the
    kernel's peer records (SO_PEERCRED, LOCAL_PEERCRED, the pipe's SID) and the identification impersonation level.
    Limits: on Windows a write that fails synchronously does not latch the pipe client (the slot stays idle and
    a later call reaches the OS again; the latch test, `rust/capsule/tests/socket_unix/client.rs`, is Unix-only); an attempt or wait in progress finishes before the bound is read (a 20 ms sleep, a 200 ms pipe wait), so
    the bound is not an exact elapsed time; Windows `connect_own` opens at identification level but cannot refuse a
    pipe before that open.
14. **A page secret is masked in the logs written through `RedactingWriter`.** `secret::redact` replaces the value after every
    `secret=` (up to the first delimiter) and every maximal run of exactly 32 lowercase hex characters with
    `<redacted>`, and leaves every other byte unchanged (borrowed when nothing matched); `RedactingWriter` applies it
    to each write and reports the whole buffer written. Wired through it: the daemon's log and the window's log. Limits:
    the supervisor's own diagnostics go straight to its stderr and are not masked; a secret split across two writes is
    not seen (each write is masked alone).

Platform facts a caller meets and may not fix (item 57 of the 0.6.7 list): an MSYS program that execs another is a
new Windows process whose Windows parent has exited, so a native parent walk alone stops short of the session (Git
Bash's `/proc/<pid>/winpid` carries the chain); a Windows console is cp437 unless the process sets UTF-8, so a prompt
glyph read back through a pipe becomes `?` and a gate comparing a pane line to it never matches; an antivirus scanner
or indexer can hold a freshly written or extracted file open for seconds, so every Windows rename and delete here
retries within `RETRY_DEADLINE_MS` and the updater's extraction retries longer; the background service on Linux is
the user manager's unit (`deploy/sotd.service`), and the runtime base is stable only while that user manager runs (linger),
so a boot without it binds sockets at `/tmp/sot-<uid>`.

## Owns
The folders and host name above (`host/state_dir.rs`); durable publication, write and remove (`host/durable.rs`);
kernel locks and held folders (`host/lock.rs`, `host/held_dir.rs`); descriptor birth and the native process-birth
launcher (`host/fd.rs`, `host/process_tree/`, one native C file); the directory pin (`host/pinned_dir.rs`); the volume
preflight; storage recognition; `Redial`; the Windows descriptors and SIDs (`host/winsec.rs`, `host/winhandle.rs`) and the anonymous kill-on-close job (`host/winjob.rs`, guarantee 12);
the private-dir and socket-dir checks; the local connection (`transport/`: `connect_own`, the two clients, the accept
policy, `Client` and `TransportError`) and the log masking (`secret.rs`); the test support every crate's tests share
(`test_wait`, `test_isolated`, `test_exec`, `test_log`, `test_scan`, `test_copy`, `test_foreign` and, under
`native-barrier`, `test_barrier`); the resource folder (`resource_dir`, `SOT_RESOURCE_ROOT`) and the verbatim-path
strip; the peer challenge's OS steps, `PeerIdentity` and `PeerProcess`, the exchange trait and the deadline
(`identity/`); the account (`os_account`) and loopback owner (`peer_owner`). The crate's `Error` is `Io`, `Path` (an OS error with what was being done) or `State`; the
backend matches `State` for a held lock (`rows/run/end_run.rs`) and `Io` for an existing target (`agents/trust_declaration.rs`),
and upper crates otherwise convert it. The resource folder and the native launcher are owned APIs without a guarantee
of their own: `resource_dir` has no test (a gap), the launcher is proved by the backend's `native_premises` tests.

## One TOML parser (stated, not yet built)
The `toml` crate is the one parser. Four hand-written readers disagree today: hosts.toml (topology), the rows
registry, the window's settings and the window's resume file. Platform offers the crate and nothing more; each owner
converts its own reader in its own pass and then deletes it.

## Calls in
The OS (renameat2, renamex_np, MoveFileEx, flock, LockFileEx, SO_PEERCRED, pidfd, kqueue, the TCP owner tables), the
C compiler for the native launcher (build time), and std, libc, windows-sys, tokio and tracing (peer_owner only),
gethostname, sha2, thiserror, getrandom; tracing-subscriber and tempfile for the test support alone.

## Calls out
Everything above; each name is a path under `sot_platform::`: `host::state_dir::{sot_state_dir, sot_config_dir,
runtime_sot_dir, host_name, kernel_host_name, state_dir_hash, is_private_dir}`, `host::{publish_noreplace,
write_durable, remove_durable, lock_writer, lock_supervisor, try_lock_daemon, HeldDir, remove_ended, preflight_volume,
storage_exhaustion, dir_identity, inherit_none, harden_own_stdio}`, `host::fd::*`, `host::winjob::{AnonymousJob, JobError}` (Windows), `host::process_tree::*`,
`host::redial::{Redial, STABLE}`, `host::env::Env`, `identity::challenge::*`, `identity::exchange::{IdentityExchange,
ExchangeDecode}`, `identity::deadline::run_with_deadline`, `identity::os_account::own_account_id`,
`identity::peer_owner::serve_own`, `transport::{Client, TransportError, CONNECT_BOUND, transport_error_to_io}`,
`transport::connect_own::connect_own`, `transport::accept_policy::{accept_policy, AcceptAction, ACCEPT_SLEEP}`,
`transport::socket_unix::SocketClient`, `transport::pipe_win::PipeClient`, `secret::{redact, RedactingWriter}`, and the shell and script twins (`sot_host`, `check_remote_fs`) which follow these
rules and are owned by them.

## Does not
Know a row, a voyage, a lane's frames or an op (the `TransportError` variants for an endpoint reached
through a daemon, and for a resolved row path, are built by those endpoints, topology's and the capsule's, never here). Decide what a challenge outcome means for readiness or adoption
(the caller does). Decide when to restart (callers pass the floor and cap). Hold connections or children. Parse
TOML.

## Platforms
Per OS: Linux (pidfd, procfs start ticks, renameat2, per-child fork guard by O_CLOEXEC), macOS (kqueue exit watch,
LOCAL_PEERPID, pidversion, the fork birth lock, renamex_np), Windows (SDDL and SID helpers, MoveFileEx,
LockFileEx, GetExtendedTcpTable, impersonation-level probe). No fourth OS builds.

## Tests
One per guarantee. Paths are under `rust/platform/src/` unless they start `tests/` (platform's own integration tests) or
`rust/capsule/`: those are tests that need the capsule's row server or its `sot-capsule` binary (a real `RowServer` answering
the hello, `CARGO_BIN_EXE_sot-capsule`), which platform cannot depend on, and stay in `rust/capsule/tests/`. A test of a
Windows or macOS arm runs on the hosted runner for its OS.

| # | guarantee | test |
|---|---|---|
| 1 | one answer, `host_name` errs | `host/state_dir_tests.rs`: `the_state_folder_has_one_rule_and_the_config_folder_is_its_sibling_rule`, `the_daemons_state_folder_is_the_state_folder_and_falls_back_to_tmp_without_a_home` (and its Windows pair), `host_name_prefers_sot_self_host_when_set` and the three `host_name_falls_back_*`/`_takes_*` cases, `a_raw_host_name_gives_its_first_lowercase_label_or_an_error`; `host/state_dir_hash_tests.rs`; `host/state_dir_runtime_dir_tests.rs`; `identity/os_account_tests.rs` |
| 2 | durable, no clobber | `host/durable_tests.rs`: `publish_noreplace_refuses_an_existing_target_and_leaves_both_files`, `write_durable_replaces_a_record_and_leaves_no_temporary_file`, `remove_durable_is_idempotent_and_accepts_a_missing_folder`, `ensure_container_*` |
| 3 | kernel lock | `host/lock_tests.rs`: `a_dropped_writer_lock_is_free_while_a_forked_child_waits_to_exec`, `a_dropped_daemon_lock_is_free_while_a_forked_child_waits_to_exec`, `a_child_spawned_while_the_daemon_lock_is_held_never_holds_it`, `try_lock_daemon_excludes_a_second_holder`, `lock_supervisor_refuses_a_second_concurrent_holder` |
| 4 | held folder | `host/held_dir_tests.rs`: `a_held_folder_goes_once_its_maker_has_ended_and_only_then`, `a_held_folder_goes_while_a_removal_holds_its_lock_file_open` |
| 5 | cloexec births | `host/fd_tests.rs` (socket, pipe, pty, the fork birth lock); `tests/winhandle_windows.rs` |
| 6 | challenge | `identity/challenge_unix_tests.rs`, `identity/challenge_identity_tests.rs`, `identity/deadline_tests.rs`, `identity/peer_owner/tests.rs`, `identity/exit_watch_macos_tests.rs`; `rust/capsule/tests/challenge_unix.rs`, `challenge_macos.rs`, `macos_kernel_facts/`; `tests/other_account.rs` |
| 7 | storage codes | `host/storage_tests.rs`: `storage_codes_are_recognized_by_native_code_only`; `rust/capsule/src/storage_tests.rs`: `a_transport_error_is_never_storage_exhaustion` |
| 8 | volume preflight | `host/volume_tests.rs` |
| 9 | redial | `host/redial_tests.rs`: `a_short_connection_keeps_the_doubling`, `a_connection_that_lasted_stable_starts_over`, `a_reset_starts_over` |
| 10 | dir pin | `host/pinned_dir_tests.rs`: `a_fifo_fails_at_once_and_never_waits_for_a_writer` |
| 11 | private dir | `host/state_dir_is_private_dir_tests.rs`; `host/paths_secure_private_dir_tests.rs`: `a_missing_parent_is_created_and_the_leaf_is_private`, `ensure_private_dir_makes_an_owner_only_folder_that_is_private` |
| 12 | job ends at last close (Windows) | `rust/capsule/tests/conpty.rs`: `containment_kills_child_and_grandchild` (`:147`); `rust/capsule/tests/capsule/windows_only.rs`: `the_agent_job_refuses_a_breakaway` (`:488`); containment's `contain_tests.rs` `contain_job_permits_no_breakaway` reads the flags only |
| 13 | local connection | `transport/accept_policy_tests.rs`; `tests/connect_own.rs`: `a_socket_this_account_listens_on_is_reached_wherever_it_is`, `a_socket_another_account_listens_on_is_refused`, `a_full_backlog_ends_the_connect_within_its_bound`, `a_missing_socket_is_not_found` (and the Windows pipe cases); the capsule's `rust/capsule/tests/socket_unix/` and `rust/capsule/tests/pipe_win/` (cancel, concurrent submit, the connector's bounds), `rust/capsule/tests/socket_unix/cloexec.rs` |
| 14 | secret masking | `secret_tests.rs`: `a_logged_secret_is_masked_in_the_file`, `only_a_run_of_exactly_32_lowercase_hex_characters_is_masked`, `one_write_call_masks_the_whole_buffer_and_reports_it_all_written` |

## Rules
- Every subscriber a test installs goes through `test_log`: `capture()` reads this thread's events without formatter
  timestamps or colour; `install()` preserves the supplied subscriber's formatting and timer. Behavioral tests exercise
  first-callsite routing, parallel captures and guard restoration; new callers use this owner. Existing exceptions, not
  edited here: the wake's `comm/wake/attempt_tests.rs` and the updater's `update_tests.rs` install a `tracing_subscriber`
  directly.
- A Rust source-text scan that exists reads through `test_scan`, and none cuts a file at its first `#[cfg(test)]`
  (`test_scan_tests.rs`: `no_scan_cuts_a_file_at_its_first_cfg_test`); an existing scan is never extended, and no new
  test proves what code does by reading source text. One exception, not edited here: `backend/tests/admission.rs` reads
  the op module's source directly (`op_names`).
