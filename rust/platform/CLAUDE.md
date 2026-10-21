# rust/platform: platform (contract)

The bottom of the workspace: one answer per machine fact and one primitive per OS mechanism, for every crate above it.
The crate (`sot-platform`) depends on nothing of ours; sot-log (the capsule), sot-protocol (wire), the backend, the
window and the updater depend on it, and nothing re-exports it. It holds no runtime state and starts no process for
anyone but the native launcher it hands out.

## Guarantees
Each states what the rest may rely on, the mechanism it rests on, and the test that holds it (the Tests section lists
where each lives).

1. **A machine fact has one answer.** The state folder (`sot_state_dir`), the config folder (`sot_config_dir`), the
   runtime base (`runtime_sot_dir`), the host name (`host_name`, `SOT_SELF_HOST` over the kernel's name), the OS
   account (`own_account_id`) and the folder hash lane sockets are named by (`state_dir_hash`) are each defined once,
   here, from an `Env` value the caller supplies or the process's own; a second rule anywhere is a defect.
   `host_name` returns `Err`, never a guessed name. A runtime base a process binds and another dials agrees only
   while the environment does not change; the supported setups keep it fixed for a boot (Linux: linger and the user
   manager's `XDG_RUNTIME_DIR`; macOS: none; Windows: no runtime base). A boot without logind binds at
   `/tmp/sot-<uid>`.
2. **A publication is durable and never clobbers.** `publish_noreplace` flushes the source, renames without
   replacing, flushes the renamed file (Windows) and then the parent; `write_durable` (tmp, fsync, replace,
   directory sync) and `remove_durable` are the one write and delete for a record a later start acts on. Rests on
   renameat2/renamex_np/MoveFileEx write-through; only linux, macos and windows build.
3. **A lock is the kernel's.** A fence, the daemon's single-instance lock and a held folder's lock are released at
   once by a dropped guard and by any death of the holder, and no exec'd child holds one (`WriterLock`'s Drop; a
   `HandoverLock` only closes, so a copy in a child keeps the fence held, by design). A contended lock fails within
   `RETRY_DEADLINE_MS` as "lock held". Kernel file locks are taken only in these guards and `InboxLock`
   (messaging's, in `rust/backend/src/comm/mail/inbox.rs`); `rust/clippy.toml` refuses them elsewhere.
4. **A held folder stays while its maker lives.** `remove_ended` removes a folder only once it can take the folder's
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
   `IdentityExchange` the lane provides), and every step is bounded by `run_with_deadline`; a wrong, partial, doubled
   or late reply is `Foreign` or `Undetermined`, never `Proven`. `connect_own` reaches only a listener this account
   owns. A TCP connection on loopback is admitted only when the OS names this account as its owner
   (`peer_owner::serve_own`). Rests on SO_PEERCRED/pidfd, LOCAL_PEERPID/kqueue, the pipe's SID and impersonation
   level, the TCP owner tables.
7. **Storage exhaustion is known by its native code.** ENOSPC and EDQUOT; on Windows ERROR_DISK_FULL,
   ERROR_HANDLE_DISK_FULL and ERROR_DISK_QUOTA_EXCEEDED, read from the `io::Error` an `Error::Io` carries, never from
   text and never from a transport error (`storage_exhaustion`). `preflight_volume` and Windows `io_ctx` return such
   an error as itself.
8. **A volume is proved before a store opens on it.** `preflight_volume` refuses a root the store cannot make
   durable, a network filesystem above all (NFS answers EINVAL to every `renameat2` flag), so a state root on a
   network home points `XDG_STATE_HOME` at local disk.
9. **A restart is paced.** A connection or child restarted through `Redial` waits from its caller's floor,
   doubling to its cap, and starts over only after one that lasted `STABLE` (60 s) or at a person's request; an
   answered hello, a bare connect or a generation alone does not restart it.
10. **A directory pin opens only a directory.** `dir_identity` opens with `O_DIRECTORY`, so a FIFO or any other
    non-directory fails at once and never waits.
11. **Private means private.** `is_private_dir` and `ensure_private_dir` accept only an owner-only directory that is
    not a link; a socket folder is secured the same way (`secure_socket_dir`).

Platform facts a caller meets and may not fix (item 57 of the 0.6.7 list): an MSYS program that execs another is a
new Windows process whose Windows parent has exited, so a native parent walk alone stops short of the session (Git
Bash's `/proc/<pid>/winpid` carries the chain); a Windows console is cp437 unless the process sets UTF-8, so a prompt
glyph read back through a pipe becomes `?` and a gate comparing a pane line to it never matches; an antivirus scanner
or indexer can hold a freshly written or extracted file open for seconds, so every Windows rename and delete here
retries within `RETRY_DEADLINE_MS` and the updater's extraction retries longer; the background service is the
platform's own supervisor (systemd user unit, launchd agent, a Windows launch supervisor), and a child it starts is
the service's, not the login session's.

## Owns
The folders and host name above (`host/state_dir.rs`); durable publication, write and remove (`host/durable.rs`);
kernel locks and held folders (`host/lock.rs`, `host/held_dir.rs`); descriptor birth and the native process-birth
launcher (`host/fd.rs`, `host/process_tree/`, one native C file); the directory pin (`host/pinned_dir.rs`); the volume
preflight; storage recognition; `Redial`; the Windows descriptors and SIDs (`host/winsec.rs`, `host/winhandle.rs`);
the private-dir and socket-dir checks; the resource folder (`resource_dir`, `SOT_RESOURCE_ROOT`) and the verbatim-path
strip; the peer challenge's OS steps, `PeerIdentity` and `PeerProcess`, the exchange trait and the deadline
(`identity/`); the account (`os_account`) and loopback owner (`peer_owner`). The crate's `Error` is `Io` or `State`;
upper crates convert, never match on it across a crate boundary.

## One TOML parser (stated, not yet built)
The `toml` crate is the one parser. Four hand-written readers disagree today: hosts.toml (topology), the rows
registry, the window's settings and the window's resume file. Platform offers the crate and nothing more; each owner
converts its own reader in its own pass and then deletes it.

## Calls in
The OS (renameat2, renamex_np, MoveFileEx, flock, LockFileEx, SO_PEERCRED, pidfd, kqueue, the TCP owner tables), the
C compiler for the native launcher (build time), and std, libc, windows-sys, tokio and tracing (peer_owner only),
gethostname, sha2, thiserror, getrandom.

## Calls out
Everything above; each name is a path under `sot_platform::`: `host::state_dir::{sot_state_dir, sot_config_dir,
runtime_sot_dir, host_name, kernel_host_name, state_dir_hash, is_private_dir}`, `host::{publish_noreplace,
write_durable, remove_durable, lock_writer, lock_supervisor, try_lock_daemon, HeldDir, remove_ended, preflight_volume,
storage_exhaustion, dir_identity, inherit_none, harden_own_stdio}`, `host::fd::*`, `host::process_tree::*`,
`host::redial::{Redial, STABLE}`, `host::env::Env`, `identity::challenge::*`, `identity::exchange::{IdentityExchange,
ExchangeDecode}`, `identity::deadline::run_with_deadline`, `identity::os_account::own_account_id`,
`identity::peer_owner::serve_own`, and the shell and script twins (`sot_host`, `check_remote_fs`) which follow these
rules and are owned by them.

## Does not
Know a row, a voyage, a lane's frames or an op. Decide what a challenge outcome means for readiness or adoption
(capsule's probe does). Decide when to restart (callers pass the floor and cap). Hold connections or children. Parse
TOML.

## Platforms
Per OS: Linux (pidfd, procfs start ticks, renameat2, per-child fork guard by O_CLOEXEC), macOS (kqueue exit watch,
LOCAL_PEERPID, pidversion, the fork birth lock, renamex_np), Windows (SDDL and SID helpers, MoveFileEx,
LockFileEx, GetExtendedTcpTable, impersonation-level probe). No fourth OS builds.

## Tests
One per guarantee. Macros of the form `crate::...` are unit tests in `rust/platform/src/`; `log/...` are integration
tests that need sot-log's test support and stay in `rust/log/tests/` until that support is its own crate.

| # | guarantee | test |
|---|---|---|
| 1 | one answer, host_name errs | `state_dir_tests.rs` (the four `host_name_*`, `sot_state_dir_is_the_one_rule`, `the_unusable_kernel_name_is_an_error`); `state_dir_hash_tests.rs`; `state_dir_runtime_dir_tests.rs` |
| 2 | durable, no clobber | `durable_tests.rs` (`publish_noreplace_refuses_an_existing_target`, `write_durable_replaces_and_leaves_no_tmp`, `remove_durable_is_idempotent`, `ensure_container_*`) |
| 3 | kernel lock | `lock_tests.rs` (`a_dropped_*_lock_is_free_while_a_forked_child_waits_to_exec`, `try_lock_daemon_excludes_a_second_holder`, `lock_supervisor_refuses_a_second_concurrent_holder`) |
| 4 | held folder | `held_dir_tests.rs` |
| 5 | cloexec births | `fd_tests.rs`; `log/tests/winhandle_windows.rs` |
| 6 | challenge | `identity/*_tests.rs`, `log/tests/challenge_unix.rs`, `challenge_macos.rs`, `connect_own.rs`, `other_account.rs`, `macos_kernel_facts` |
| 7 | storage codes | `storage_tests.rs` |
| 8 | volume preflight | `volume_tests.rs` |
| 9 | redial | `redial_tests.rs` |
| 10 | dir pin | `pinned_dir_tests.rs` |
| 11 | private dir | `state_dir_is_private_dir_tests.rs`; `private_dir_tests.rs` |
