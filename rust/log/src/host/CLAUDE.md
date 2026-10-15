# rust/log/src/host: platform (charter)

## Idea
One answer per machine fact and one primitive per platform mechanism: where state lives, what the host is called, how a
file is published durably, how a lock is held, what a volume must support, and how soon a dropped connection or a dead child is started again. It lives in sot-log because that is the
workspace's bottom crate: every other Rust crate can reach it and it reaches none of them.

## Owns
- The per-machine dirs and the host name (`state_dir.rs`: `sot_state_dir`, `sot_config_dir`, `runtime_sot_dir`, `runtime_dir`,
  `state_dir_hash`, `host_name`, and `kernel_host_name`, the kernel's name without the `SOT_SELF_HOST` override).
- Publication and fsync (`durable.rs`: `publish_noreplace`, `finish_publication`, `rename_noreplace_raw`, `fsync_dir`,
  `fsync_file`, `ensure_container`, `create_dir_protected`).
- Kernel locks (`lock.rs`: `lock_writer`, `lock_supervisor`, and for a fence handed to a child `lock_supervisor_for_handover`,
  `HandoverLock`).
- Held folders (`held_dir.rs`: `HeldDir`, `remove_ended`): a folder that lives while the process that made it does,
  held by its lock (`lock.rs`: `lock_new`, and `lock_held` for a removal's take).
- The native process-birth primitives (`process_tree/`, its own page): the two-phase launcher.
- The daemon's single-instance lock (`lock.rs`: `daemon_lock_path`, `try_lock_daemon`, `DaemonLock`), reached by the backend
  as `sot_log::host::daemon_lock_path`, `try_lock_daemon` and `DaemonLock`.
- The volume preflight (`volume.rs`: `preflight_volume`).
- The directory pin (`pinned_dir.rs`: `PinnedDir`, `DirIdentity`, `dir_identity`).
- Windows owner-only descriptors and SIDs (`winsec.rs`: `owner_protected_pipe_descriptor`, `token_user_sid_string`).
- Stdio inherit hardening (`winhandle.rs`: `harden_own_stdio`; Unix `inherit.rs`: `inherit_none`, the pre-exec sweep that marks every descriptor above stdio close-on-exec, called by the daemon's children, the capsule's leg and every ssh child (topology's `ssh_bridge`, the window's and, through `LinkGate::command`, the daemon's)).
- The retry deadline and error context every file here shares (`mod.rs`: `RETRY_DEADLINE_MS`, `io_ctx`,
  `duration_to_wait_ms`).
- The peer challenge in `rust/log/src/identity/` (a sibling folder; see Folders).
- Storage-exhaustion recognition (`storage.rs`: `storage_exhaustion`, `native_storage_code`).
- The redial pace of a long-lived connection or child (`redial.rs`: `Redial`, `STABLE`), which the window's control
  transport, the hub link, the attach worker and the kernel supervisor share.

## Promises
- `host_name` returns `Err`, never a guessed name.
- Every process resolves the runtime base itself, from its own environment (`runtime_sot_dir`); `runtime_dir` adds
  only `SOT_RUNTIME_DIR`, which no production code sets and which moves only the capsule lane sockets. A path one
  process binds and another dials or copies (the session socket and the `SOT_SOCKET` each row is spawned with, the
  lane sockets) agrees only while the base does not change. The supported setups keep it fixed for a boot (Linux:
  linger and the user manager's `XDG_RUNTIME_DIR`; macOS: no `XDG_RUNTIME_DIR`; Windows has no runtime base). A boot
  without logind binds at `/tmp/sot-<uid>`; a daemon that takes over after `/run/user/<uid>` appears cannot reach the
  rows that boot spawned.
- `dir_identity` opens only a directory (`O_DIRECTORY` on Unix), so a path that names a FIFO or any other non-directory
  fails at once and never waits.
- `preflight_volume` refuses a root the store cannot make durable: a network filesystem (NFS answers EINVAL to every
  `renameat2` flag the store publishes with), so a state root on a network home must point `XDG_STATE_HOME` at local
  disk.
- Only linux, macos and windows build: `rename_noreplace_raw` has three arms and no fourth.
- Publication is source flush, no-clobber rename, renamed-file flush on Windows, then parent flush
  (`publish_noreplace`, `finish_publication`).
- A lock is kernel-held: a dropped guard unlocks it at once (`WriterLock`'s Drop), the kernel releases it on any death, and no exec'd child holds it; a contended one fails within `RETRY_DEADLINE_MS` as "lock held"
  (`lock_writer`).
- A held folder stays while its maker lives, wherever the maker runs: `remove_ended` removes a folder only once it can
  take the folder's lock, which `HeldDir::create` takes under a staging name before it renames the lock file to the
  name a removal looks for, and which the maker holds until it ends, so another computer sharing the folder, another
  pid namespace or a pid the OS has reused never makes a live maker's folder go. A folder whose lock is held, missing
  or cannot be opened, a link, a file and another account's folder stay. Across computers this needs a file system
  whose locks every client sees (NFS with its lock service, not one mounted `nolock` or with `local_lock`), and only
  while the maker's client holds its lease: an NFS client cut off from the server past its lease (90 s by default) can
  lose its lock, and another computer's start can then remove its live folder. A temporary folder on a file system that
  refuses locks (NFSv3 without its lock service answers ENOLCK) stops `HeldDir::create`, so no Julia child starts and
  every release check fails with "making the release check's scratch folder". A removal waiting on a live maker's lock
  never stops the maker removing its folder: on Windows a held folder's lock file is opened sharing deletion, by its
  maker (`lock_new`) and by a removal (`lock_held`), where a fence's lock file, never removed, is not (`lock_writer`).
- Kernel file locks are taken only inside three guards: `WriterLock` in `lock.rs` here and `InboxLock` in the backend's `rust/backend/src/comm/mail/inbox.rs`, whose `Drop` unlocks, and `HandoverLock` in `lock.rs`, whose drop only closes, so a copy of its descriptor in a child keeps the lock held (a birth's claim on a row's fence). rust/clippy.toml disallows `File`'s lock methods and `libc::flock` everywhere else.
- The challenge's OS steps precede its wire steps and every step is bounded (`identity/`).
- A connection or child restarted through `Redial` waits from its caller's floor, doubling to its cap, and starts over
  only after one that lasted `STABLE` (60 s), or when its caller resets it at a person's request (the window's F5); an
  answered hello, a completed attach, a bare connect or a kernel generation alone does not restart it. The window and
  the hub link measure from the attempt's start, the attach episode from its attach and the kernel from its hello (a
  precompile that never answers counts as nothing), each waiting from the session's end; the supervisor re-dial
  measures from its previous dial (`dialed_at`) and waits from there, so a lane that lasted longer than its current
  wait re-dials at once.
- Storage exhaustion is recognized by its native code only: ENOSPC and EDQUOT, on Windows ERROR_DISK_FULL, ERROR_HANDLE_DISK_FULL and ERROR_DISK_QUOTA_EXCEEDED, read from the `io::Error` an `Error::Io` carries, never from text and never from a transport error (a full runtime folder is not storage exhaustion) (`storage_exhaustion`). `preflight_volume` and Windows `io_ctx` return such an error as itself, with its code, instead of their refusal or context text.

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: `sot_state_dir`, `sot_config_dir`,
`host_name`, `kernel_host_name`, `state_dir_hash`, `publish_noreplace`, `lock_writer`, `try_lock_daemon`, `preflight_volume`,
`owner_protected_pipe_descriptor`, `harden_own_stdio`, `inherit_none`, `boot_identity`, `process_created`, `IdentityExchange`,
`durable::write`, `durable::remove`, `rust/backend/src/durable.rs`, `dir_identity`,
`rust/log/src/host/pinned_dir.rs`, `resource_dir`, `rust/backend/src/paths.rs`,
`sot_host`, `comm/lib/comm-lib-base.sh`, `check_remote_fs`, `scripts/install.sh`, `REMOTE_FS_TYPES`, `storage_exhaustion`,
`Redial`, `STABLE`, `HeldDir`, `remove_ended`. Uses: none.

## Folders
- `rust/log/src/host/` (here) and `rust/log/src/identity/` (the peer challenge).

## Files
- `mod.rs`: the module list, the shared retry constants, `io_ctx`, `duration_to_wait_ms`
  and the glob re-exports of the files below.
- `durable.rs`: durable publication, fsync, no-clobber rename, container creation.
- `inherit.rs`: Unix-only `inherit_none`, the pre-exec sweep that leaves a child only its stdio and what its caller hands it; the daemon's and capsule's children and every ssh child (the window's, and the daemon's through `LinkGate::command`) call it.
- `held_dir.rs`: held folders, each live while the process that made it is (`HeldDir`, `remove_ended`).
- `lock.rs`: the writer fence, the supervisor fence, the supervisor fence taken to be handed to a child, the daemon's single-instance lock and a held folder's lock, held by the kernel.
- `pinned_dir.rs`: a directory's kernel identity and a handle that pins it.
- `process_tree/`: the native process-birth primitives (own page).
- `redial.rs`: `Redial` and `STABLE`, the wait before a long-lived connection or child is started again.
- `state_dir.rs`: where a file lives, and the host name.
- `storage.rs`: which native errors are storage exhaustion.
- `volume.rs`: the preflight that proves a volume supports the store's primitives.
- `winhandle.rs`: Windows-only hardening of a process's own inherited stdio handles.
- `winsec.rs`: Windows-only owner-only security descriptors and SID lookups, and `wide_null`, the UTF-16 form of a non-path string.

## Start here
`durable.rs` `publish_noreplace` for how a file is published; `state_dir.rs` for where a file lives.
