# rust/backend/src/lifecycle: lifecycle (charter)

## Idea
Every process the daemon starts, except those ADR 0050 names as outside, runs in its own containment. Owner release or the process-wide signal attempts termination of that contained tree and reports request failures; successful requests do not establish death before return or daemon exit. A start and the signal share the tree-registry mutex through creation and registration. Row shutdown has an OS-clock deadline; an OS creation or adoption that never returns can delay fire and process exit. The last window on a computer decides, through its lease, whether that computer's sessions end (ADR 0050).

## Owns
- Unix SIGINT and SIGTERM delivery: `signal_exit::install`, with handlers registered and a checked unblocked watcher mask before daemon child work.
- The daemon's terminal function: `shutdown::exit`, which fires before the sole raw process exit.
- The window leases and `<state>/held.json`: `Leases`, `read_record`, `write_or_delete` (`crate::lifecycle::lease`, the file
  `lease.rs` here).
- The lease ops `fe.lease`, `fe.leaving`, `fe.notice_seen` (`lease::hold`) and the 1 s `lease::ticker`.
- The start plan Resume, Pending or Cleanup: `startup::begin`, `lease::startup_plan`.
- The close and its backstop `exit(1)`: `shutdown::run`, `shutdown::end_rows`.
- The child signal and containment: `Signal`, `Signal::spawn`, `Signal::spawn_std`, `Signal::output`, `Contained`, `ContainedStd`, `ContainedStd::wait_within`, `fire`, `fired`, `reset_child_signal`; contain.rs `Tree`, `Tree::terminate`, `prepare`, `adopt`, `exited`, `exited_pid`.
- Which process starts stand outside the containment: the process-spawns group of `rust/clippy.toml` and each
  exception's allow.
- The bounds and exit codes in `sot_protocol::ops::lease`.
- The window's half, rust/frontend/src/lease.rs.

## Promises
- Unix SIGINT and SIGTERM are observed by a watcher on an independent runtime thread and call `shutdown::exit(130)` and `shutdown::exit(143)` respectively. Installation registers both handlers and checks the watcher's unblocked mask before daemon child work, including inherited blocked masks. Installation, mask or read failure is a visible failure exit. No registry lock is taken inside a POSIX signal handler.
- Every controlled daemon termination uses `shutdown::exit`: requested close, backstop, update restart, explicit boot/command refusal, main result and main-future unwind. It synchronously attempts and checks every contained-tree termination request before process exit; errors are logged and the chosen exit code is preserved. It does not wait for confirmed tree death. Uncatchable signals, aborts and process/OS crashes cannot execute this cleanup.
- A lease is granted only to a peer whose pid, creation time and boot equal what the OS reported at accept
  (`lease::claim`, called by `Leases::grant`).
- Deadlines are wall-clock unix milliseconds, so a persisted handover deadline survives a restart (`startup_plan`
  reads `handover_until_ms` as written).
- `held.json` is deleted when every field is empty or false and otherwise written through `crate::durable`
  (`write_or_delete`).
- The start plan is a function of the record and this boot alone, never of a process lookup (`startup_plan`); an
  unreadable record, a closing record, another boot or an expired handover plans Cleanup.
- Cleanup ends every row and resumes none (`startup::cleanup`); Resume and Pending resume rows at once
  (`rows::run::resume::resume_all`).
- The close stops accepting before it touches a row (the accept loop in `server::run` breaks on `Leases::gone`, drops
  the listener, then calls `shutdown::run`), ends rows without resuming any (`end_rows`), counts each row not confirmed
  ended. The backstop requests exit 1 at `SHUTDOWN_BOUND`; an OS creation or adoption stalled under the start/registry mutex can delay fire and actual process exit.
- A close that finishes exits 0 (`bounds::EXIT_REQUESTED_SHUTDOWN`); the update restart exits 75 and only while no
  shutdown has begun (`Leases::while_open`, called by update.rs).
- `fire()` is permanent: the signal is never reset for the life of the process (`Signal::fire`).
- A child started through `Signal::spawn` or `Signal::spawn_std` receives checked tree-termination attempts when its owner kills, waits for or drops its `Contained` or `ContainedStd`, or the signal fires. Explicit operations propagate cleanup errors; Drop logs them. On Unix, successful termination requests precede the direct-child reap, preserving the process-group identity. On Windows, `Contained::wait` obtains the direct-child status with `child.wait()` before requesting job termination; blocking `ContainedStd` exit observation uses `child.wait()` and nonblocking observation uses `child.try_wait()` before the subsequent job request. The retained job handle preserves containment identity across those Windows waits. Explicit kill requests termination before waiting on either platform. The contained owner remains responsible for direct-child reap; fire neither reaps nor proves tree death before return or daemon exit. Neither type hands its caller the child to reap (`Contained::wait`, `ContainedStd::wait`; Unix `exited_pid` uses `WNOWAIT`). Creation through adoption and registration holds the registry mutex that `Signal::fire` takes. A start after fire creates nothing; fire cannot return past an unregistered start. An OS creation or adoption that never returns can therefore delay fire and process exit. There is no child-count grace period. On macOS only, a real group-request EPERM counts as no live member only after checked observation confirms that the retained leader has exited without being reaped and a complete libproc process-group membership/status query finds no live member; live, failed or ambiguous observations preserve the original error, and leader requests and injected failures remain independently checked. No Unix signal or process-group identity query occurs after leader reap.
- `ContainedStd::wait_within` bounds waiting for a live leader. At timeout, `Ok(None)` confirms successful tree-termination requests and a direct-child reap; it does not confirm descendant death. Probe/request/reap errors are returned, preserving both probe and cleanup reasons when both fail. OS termination/reap is not given a wall-clock ceiling.
- `main` resets `SIGCHLD` to its default and unblocks it in the main thread before anything else (`reset_child_signal`),
  so neither an ignored nor a blocked one inherited from the parent can make the kernel reap a contained leader early or
  keep `Contained::wait` from seeing its exit; the main thread lives as long as the daemon, so the signal always has a
  thread to reach.
- Every call of a function in `rust/clippy.toml`'s process-spawns group sits in `Signal::spawn`, `Signal::spawn_std`
  or `Signal::output`, or in a statement whose `#[allow(clippy::disallowed_methods)]` gives its reason: rust.yml's
  "Disallowed methods" step fails on any other. The group holds every way std and tokio start a process, portable-pty's
  `spawn_command`, libc's `fork`, `vfork`, `posix_spawn`, `posix_spawnp`, `execv`, `execve`, `execvp` and `system`,
  windows-sys's `CreateProcessW`, `CreateProcessA`, `CreateProcessAsUserW` and `CreateProcessAsUserA`, `LinkGate`'s
  `spawn_sync`, `spawn_async` and `probe`, and no updater entry: the updater requires its caller's `Spawner` and starts no process directly (rust/updater/CLAUDE.md). Not held:
  - other process starts in libc and windows-sys, among them libc's other exec, fork and spawn functions and `popen`,
    and windows-sys's `CreateProcessWithLogonW`, `CreateProcessWithTokenW`, `WinExec`, `ShellExecute*`,
    `SHCreateProcessAsUserW` and `SHOpenWithDialog`, called nowhere in the workspace today (libc:
    `rust/backend/Cargo.toml:37`, `rust/log/Cargo.toml:54`, `rust/updater/Cargo.toml:19`; windows-sys features: `rust/backend/Cargo.toml:55`,
    `rust/frontend/Cargo.toml:107-114`, `rust/log/Cargo.toml:72-84`);
  - a start inside any other dependency (for example winresource's resource compile in `rust/frontend/build.rs`), or a
    raw `syscall`;
  - sot-log's own starts, reached through its public items (rust/log/CLAUDE.md);
  - the lane client's ssh, which a `LaneDial::Ssh` reaches through sot-log's `Endpoint` trait (only the window builds
    one today).
- On Windows a child holds only the handles its owner hands it, as far as the workspace makes them: std creates its
  sockets, files and pipes non-inheritable; no socket or pipe is made through a constructor `rust/clippy.toml` lists for
  this rule (the inheritable-handles group, the two raw-security-attribute constructors it names under accepts and
  local endpoint dials, and `tokio::net::TcpStream::connect` under TCP dials) but at a statement whose allow says
  why; every accepted socket is cleared at accept (`serve_own`); the daemon and the window clear their own inherited standard handles first (`harden_own_stdio`); and the browser opener and `gio trash` are handed null standard handles (`spawn_opener`, `trash_file`), because std passes an inherited standard handle to a child as an inheritable copy.
  - Not held for this rule, and non-inheritable at every call today:
    - windows-sys's `CreatePipe` (one call, null attributes, `rust/log/src/capsule/producer/conpty/mod.rs:171`);
    - the constructors that take security attributes (`CreateFileW`, `CreateNamedPipeW` and their kin, held by the
      local-dials and accepts groups for their own rules; every call passes null attributes or `bInheritHandle: 0`);
    - interprocess's `PipeListenerOptions` (its `inheritable` option is false by default and never set,
      `rust/backend/src/server/listen.rs:180`);
    - handles of other kinds (process, thread, event, mutex, job; every call asks for no inheritance);
    - `SetHandleInformation`, called only to clear the flag (`rust/log/src/identity/peer_owner/mod.rs:95`,
      `rust/log/src/host/winhandle.rs:29`).
  - Not held, and called by no Windows code today, among them: interprocess's other unnamed-pipe constructors
    (`unnamed_pipe::tokio::pipe`, the Windows `CreationOptions`), `DuplicateHandle`, and libc's CRT openers on
    windows-gnu (`open`, `pipe`, `socket`, `dup`).
  - Not reached, and closable only by a handle list at spawn, which stable std lacks: a handle a process inherited from
    its starter beyond its three standard handles, a handle a dependency makes (the window's graphics, clipboard and
    dialog libraries), and a socket accepted in the moment before `serve_own` clears it, if a child starts on another
    thread in that moment.
  - On Unix std makes every descriptor close-on-exec, but sot-log's lane socket connector sets the flag in a second
    call after making the socket (`rust/log/src/lane/socket_unix/connect.rs:73`, `:80`), so an exec on another thread
    between the two calls inherits it.
- A window started with `--ephemeral`, `--capture` or `--no-lease` never leases (the frontend's `lease_exempt`).

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: `signal_exit::install`, `shutdown::exit`, `startup::begin`, `lease::ticker`,
`Leases::gone`, `shutdown::run`, `fe.lease`, `fe.leaving`, `fe.notice_seen`, `rust/frontend/src/lease.rs`,
`Leases::before_data_connection`, `scripts/sot-lease.ps1`, `Leases::while_open`, `Signal::spawn`, `Signal::spawn_std`,
`Signal::output`, `Contained`, `ContainedStd`, `ContainedStd::wait_within`, `Signal`, `child_signal::fired`, `child_signal::process`. Uses: `sot_updater::Spawner`, `AnonymousJob`, `fe.lease`, `handle_connection`, `lease::hold`, `admit_peer`,
`reject`, `write_frame_within`, `write_frame_to`, `destroy_capsule_workspace`, `end_default_row_run`, `resume_all`,
`close_gate_and_settle`, `remove_row_files`, `sot_state_dir`, `sot_config_dir`, `host_name`, `state_dir_hash`,
`durable::write`, `durable::remove`, `rust/backend/src/durable.rs`, `deploy/sotd.service`, `sot-apply.sh`.

## Folders
- `rust/backend/src/lifecycle/`: this folder.
- `rust/frontend/src/lease.rs`: a file, the window's half.

## Files
- `child_signal.rs`: the process-wide signal, the tree registry, synchronized child creation and registration, and the contained children (`Contained`, `ContainedStd`).
- `contain.rs`: platform containment, adoption, checked termination requests and platform-specific exit observations (unreaped on Unix, wait/try_wait on Windows).
- `exit_tests.rs`: terminal-body request ordering and main completion paths; a lexical pin checks exit/abort/_exit/TerminateProcess/ExitProcess tokens (except fn exit, .abort and shutdown::exit), the std::process::* and libc::* spellings, the sole permitted raw exit and the main-boundary text. The pin misses namespace aliases named shutdown, renamed primitives whose imports lack those tokens, and other termination mechanisms.
- `start_tests.rs`: suspended-start and ready-tree registration races, plus checked cleanup failures.
- `lease.rs`: the window lease: `Leases`, the grant rule, the lease connection (`hold`), `held.json` and the start plan.
- `lease_tests.rs`: tests of the grant rule, departures and ticks, held.json, the start plan and the lease connection.
- `mod.rs`: declares the lifecycle modules.
- `signal_exit.rs`: the Unix termination watcher, its independent runtime thread and startup registration/mask/error handoff.
- `shutdown.rs`: the close, its backstop, the row ends and `exit`, the one fire-before-termination function.
- `startup.rs`: the start's decision from `held.json` and acting on it.

## Start here
`startup::begin` for what a start does; `shutdown::run` for the close and its order; `child_signal.rs` before a change
to how a child is owned.
