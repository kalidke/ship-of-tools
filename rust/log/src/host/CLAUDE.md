# rust/log/src/host: platform (charter)

## Idea
One answer per machine fact and one primitive per platform mechanism: where state lives, what the host is called, how a
file is published durably, how a lock is held, and what a volume must support. It lives in sot-log because that is the
workspace's bottom crate: every other Rust crate can reach it and it reaches none of them. `lib.rs` aliases
`host as fsutil` and re-exports `host::state_dir` and `host::winhandle`, so the old `crate::fsutil::...` and
`sot_log::state_dir` paths still resolve until the crate's re-export cleanup.

## Owns
- The per-machine dirs and the host name (`state_dir.rs`: `sot_state_dir`, `sot_config_dir`, `runtime_dir`,
  `state_dir_hash`, `host_name`).
- Publication and fsync (`durable.rs`: `publish_noreplace`, `finish_publication`, `rename_noreplace_raw`, `fsync_dir`,
  `fsync_file`, `ensure_container`, `create_dir_protected`).
- Kernel locks (`lock.rs`: `lock_writer`, `lock_supervisor`, `try_lock_bounded`).
- The volume preflight (`volume.rs`: `preflight_volume`).
- The directory pin (`pinned_dir.rs`: `PinnedDir`, `DirIdentity`, `dir_identity`).
- Windows owner-only descriptors and SIDs (`winsec.rs`: `owner_protected_pipe_descriptor`, `token_user_sid_string`).
- Stdio inherit hardening (`winhandle.rs`: `harden_own_stdio`).
- The retry deadline and error context every file here shares (`mod.rs`: `RETRY_DEADLINE_MS`, `io_ctx`,
  `duration_to_wait_ms`).
- The peer challenge in `rust/log/src/identity/` (a sibling folder; see Folders).

## Promises
- `host_name` returns `Err`, never a guessed name.
- `preflight_volume` refuses a root the store cannot make durable: a network filesystem (NFS answers EINVAL to every
  `renameat2` flag the store publishes with), so a state root on a network home must point `XDG_STATE_HOME` at local
  disk.
- Only linux, macos and windows build: `rename_noreplace_raw` has three arms and no fourth.
- Publication is source flush, no-clobber rename, renamed-file flush on Windows, then parent flush
  (`publish_noreplace`, `finish_publication`).
- A lock is kernel-held and released on any death; a contended one fails within `RETRY_DEADLINE_MS` as "lock held"
  (`lock_writer`).
- The challenge's OS steps precede its wire steps and every step is bounded (`identity/`).

## Connections
- In this crate: the store, supervisor, lanes, conpty and capsule call `publish_noreplace`, `lock_writer`, `PinnedDir`
  and `state_dir`.
- Outside: `sot_log::state_dir` in the backend, frontend and protocol (about 75 uses); `sot_log::lock_writer` in the
  backend's leg-absence proof; `sot_log::owner_protected_pipe_descriptor` for the daemon's session pipe;
  `winhandle::harden_own_stdio` in the daemon's main.
- Beside it: the backend's own platform files `rust/backend/src/paths.rs` and `rust/backend/src/durable.rs`.
- Known twins: the backend's `paths.rs` state-root rule, the shell copies of the host-name rule, and the copy of
  `REMOTE_FS_TYPES` in `scripts/install.sh` (`check_remote_fs`).

## Folders
- `rust/log/src/host/` (here) and `rust/log/src/identity/` (the peer challenge; its folder is not at this commit, so
  this is a forward reference).

## Files
- `mod.rs`: the module list, the shared retry constants, `io_ctx`, `duration_to_wait_ms`
  and the re-exports that keep the old `fsutil` paths.
- `durable.rs`: durable publication, fsync, no-clobber rename, container creation.
- `lock.rs`: the writer fence and the supervisor fence, held by the kernel.
- `pinned_dir.rs`: a directory's kernel identity and a handle that pins it.
- `state_dir.rs`: where a file lives, and the host name.
- `volume.rs`: the preflight that proves a volume supports the store's primitives.
- `winhandle.rs`: Windows-only hardening of a process's own inherited stdio handles.
- `winsec.rs`: Windows-only owner-only security descriptors and SID lookups.

## Start here
`durable.rs` `publish_noreplace` for how a file is published; `state_dir.rs` for where a file lives.
