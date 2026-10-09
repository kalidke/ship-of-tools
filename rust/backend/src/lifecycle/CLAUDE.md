# rust/backend/src/lifecycle: lifecycle (charter)

## Idea
Nothing outlives its owner unless designed to: every process the daemon starts, but those ADR 0050 names as outside,
runs in its own containment, which its owner's release or the process-wide signal kills with everything it started,
every exit is bounded on the OS clock, and the last window on a computer decides, through its lease, whether that
computer's sessions end (ADR 0050).

## Owns
- The window leases and `<state>/held.json`: `Leases`, `read_record`, `write_or_delete` (`crate::lifecycle::lease`, the file
  `lease.rs` here).
- The lease ops `fe.lease`, `fe.leaving`, `fe.notice_seen` (`lease::hold`) and the 1 s `lease::ticker`.
- The start plan Resume, Pending or Cleanup: `startup::begin`, `lease::startup_plan`.
- The close and its backstop `exit(1)`: `shutdown::run`, `shutdown::end_rows`.
- The serving daemon's controlled ends and what each does first: `shutdown::exit` (the one raw exit),
  `shutdown::exit_by_signal` (INT and TERM: the same fire, then the signal), `shutdown::terminal`; INT and TERM:
  `signal_exit::install`.
- The child signal and the containment: `Signal`, `Signal::spawn`, `Signal::spawn_std`, `Signal::output`,
  `Contained`, `Contained::wait_until_exited`, `ContainedStd`, `Signal::fire`, `reset_child_signal`; contain.rs `Tree`,
  `prepare`, `adopt`,
  `exited`, `exited_pid`.
- Which process starts stand outside the containment: the process-spawns group of `rust/clippy.toml` and each
  exception's allow.
- The Linux lifetime guard: `daemon_children::guard` (`require_one_thread`, `install`, `drain`, `guard_pid`).
- The bounds and exit codes in `sot_protocol::ops::lease`.
- The window's half, rust/frontend/src/lease.rs.

## Promises
- The window's forced deliver_queued wait covers writes already queued to all holders with one std::time::Instant deadline; it does not wait for daemon acknowledgements and its deadline does not depend on the transport worker.
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
  ended, and a backstop thread exits 1 at `bounds::SHUTDOWN_BOUND` (`shutdown::run`, step 0).
- A close that finishes exits 0 (`bounds::EXIT_REQUESTED_SHUTDOWN`); the update restart exits 75 and only while no
  shutdown has begun: update.rs commits it with `Leases::commit_update`, which moves the phase to `Updating` under the
  lease lock, so no close begins afterwards, and exits outside the lock.
- Every controlled end of the serving daemon takes one terminal, `shutdown::exit`: it fires the child signal (each
  contained tree is asked to end), waits at most `FIRE_WAIT` (2 s) for the answer, logs a failed request or a fire still
  running, and makes the process's one raw exit with the code it was given. The codes: 0 (a finished close, an Ok main
  result), 1 (an error, a boot refusal, the backstop), 2 (a bad `agent-exec` recipe), 75 (the update restart), 78 (no
  config directory), 101 (a panic of the main future); INT and TERM take the same fire and then end the daemon by the
  signal itself with its default action (`shutdown::exit_by_signal`), so a service manager counts the stop as clean and a
  shell reads 130 and 143; a death by an uncatchable signal, an abort or a raw exit elsewhere is the guard's and the OS's,
  not this terminal's. `main` turns the main future's result into
  its code while the runtime still exists (`complete_main`). INT and TERM are caught on a thread of their own with a
  runtime of its own, unblocked whatever mask the daemon inherited and checked to be deliverable, so a stalled main runtime
  does not hold them; a failed installation refuses the boot (`signal_exit::install`). The wait is bounded because a child
  creation stalled in the OS holds the registry mutex the fire needs: no exit, the backstop's included, depends on it. A
  request is not a death: on Linux the guard ends what is left, and on macOS the controlled ends are the only ones that end
  the daemon's children (ADR 0050).
- On Linux a process the daemon starts, at any depth, ends within `DRAIN_BOUND` of the daemon's end, however the daemon
  ends, unless a broker started it, the guard itself was killed, or a kernel call is uninterruptible: every serving
  daemon is the child of a guard that is a subreaper and kills its own children until it has none, then exits as the
  daemon did (`daemon_children::guard`). A capsule is outside it by design, and the durable parent is born before the
  guard, so it never descends from it.
- `fire()` is permanent: the signal is never reset for the life of the process (`Signal::fire`).
- `Contained::wait_until_exited` observes its owned direct child's exit without releasing containment or reaping it. Cancelling that wait retains the child's identity and owner; checked wait/kill still request tree termination before direct-child reap.
- A child started through `Signal::spawn` or `Signal::spawn_std` dies with everything it started when its owner
  kills, waits for or drops its `Contained` or `ContainedStd`, or the signal fires; its leader is reaped only after
  that kill, and neither type hands its caller the child to reap (`Contained::wait`, `ContainedStd::wait`;
  `exited_pid` uses `WNOWAIT`). Creation through adoption and registration holds the registry mutex that `fire` takes
  (`Signal::reserve`, `Provisional`, `Held::fill`): a start is refused before anything is created once the signal has
  fired, and a child created while it fires is either registered and requested or cleaned up by its provisional owner,
  on an error or an unwind too. `fire` attempts every registered tree, reports every failed request and waits for no
  death and no child count; the shutdown has no grace period. An OS creation or adoption that never returns holds the
  mutex and so delays `fire`: that is the stated kernel limit.
- `ContainedStd::wait_within` bounds the wait for a live leader. At the bound `Ok(None)` confirms successful
  tree-termination requests and a direct-child reap; it does not confirm descendant death. Probe, request and reap errors
  are returned, both reasons kept when a probe and a cleanup both fail. An OS termination or reap has no wall-clock ceiling.
- A request is checked: a group and its unreaped leader are asked independently, only ESRCH counts as already gone,
  and the owner reaps the leader only after the requests succeed. On macOS alone a group request refused with EPERM
  counts as no live member only when the retained leader is seen exited unreaped and a complete libproc membership and
  status query, taken twice, finds every member a zombie of that group (`contain::macos::checked_no_live_group`); a live
  member, a failed or an ambiguous observation keeps the original error.
- `main` resets `SIGCHLD` to its default and unblocks it in the main thread before anything else (`reset_child_signal`),
  so neither an ignored nor a blocked one inherited from the parent can make the kernel reap a contained leader early or
  keep `Contained::wait` from seeing its exit; the main thread lives as long as the daemon, so the signal always has a
  thread to reach.
- Every call of a function in `rust/clippy.toml`'s process-spawns group sits in `Signal::spawn`, `Signal::spawn_std`
  or `Signal::output`, or in a statement whose `#[allow(clippy::disallowed_methods)]` gives its reason: rust.yml's
  "Disallowed methods" step fails on any other. The group holds every way std and tokio start a process, portable-pty's
  `spawn_command`, libc's `fork`, `vfork`, `posix_spawn`, `posix_spawnp`, `execv`, `execve`, `execvp` and `system`,
  windows-sys's `CreateProcessW`, `CreateProcessA`, `CreateProcessAsUserW` and `CreateProcessAsUserA`, `LinkGate`'s
  `spawn_sync`, `spawn_async` and `probe`, and no updater entry: the updater requires its caller's `Spawner` and starts no process itself (rust/updater/CLAUDE.md). Not held:
  - other process starts in libc and windows-sys, among them libc's other exec, fork and spawn functions and `popen`,
    and windows-sys's `CreateProcessWithLogonW`, `CreateProcessWithTokenW`, `WinExec`, `ShellExecute*`,
    `SHCreateProcessAsUserW` and `SHOpenWithDialog`, called nowhere in the workspace today (libc:
    `rust/backend/Cargo.toml:37`, `rust/log/Cargo.toml:54`, `rust/updater/Cargo.toml` (its unix dependencies, from
    R9); windows-sys features: `rust/backend/Cargo.toml:55`,
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
  why; every accepted socket is cleared at accept
  (`serve_own`); the daemon and the window clear their own inherited standard handles first (`harden_own_stdio`); and
  the browser opener and `gio trash` are handed null standard handles (`spawn_opener`, `trash_file`), because std
  passes an inherited standard handle to a child as an inheritable copy.
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
- A Foreign lease outcome names a refused identity claim, not an absent backend or a proved different OS account; notice precedence is Undetermined, Unsupported, Foreign, then Unreached after granted/pending/exempt suppression.

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: `startup::begin`, `lease::ticker`,
`Leases::gone`, `shutdown::run`, `fe.lease`, `fe.leaving`, `fe.notice_seen`, `rust/frontend/src/lease.rs`,
`Leases::before_data_connection`, `scripts/sot-lease.ps1`, `Leases::commit_update`, `shutdown::exit`, `signal_exit::install`, `Signal::spawn`, `Signal::spawn_std`,
`Signal::output`, `daemon_children::guard`, `guard_pid`, `Contained`, `Contained::wait_until_exited`, `ContainedStd`, `ContainedStd::wait_within`, `Signal`, `child_signal::process`, lease_notice. Uses: `AnonymousJob`, `fe.lease`, `handle_connection`, `lease::hold`, `admit_peer`,
`reject`, `write_frame_within`, `write_frame_to`, `destroy_capsule_workspace`, `end_default_row_run`, `resume_all`,
`close_gate_and_settle`, `remove_row_files`, `sot_state_dir`, `sot_config_dir`, `host_name`, `state_dir_hash`,
`durable::write`, `durable::remove`, `rust/backend/src/durable.rs`, `deploy/sotd.service`, `sot-apply.sh`, Dial.

## Folders
- `rust/backend/src/lifecycle/`: this folder.
- `rust/frontend/src/lease.rs`: a file, the window's half.

## Files
- `child_signal.rs`: the process-wide signal, the registry of contained trees and its creation mutex, the provisional
  owner of a created child, and the contained children (`Contained`, `ContainedStd`), with the observation-only
  `Contained::wait_until_exited`.
- `daemon_children/`: what ends with a daemon: the Linux lifetime guard (see its page).
- `contain.rs`: the platform half of containment: the process group or job, adopting a child, the checked kill, and on
  macOS the recognition of a finished group.
- `lease.rs`: the window lease: `Leases`, the grant rule, the lease connection (`hold`), `held.json` and the start plan.
- `lease_tests.rs`: tests of the grant rule, departures and ticks, held.json, the start plan and the lease connection.
- `mod.rs`: declares the seven modules, the held points (feature `daemon-lifetime-faults`) and the test module.
- `shutdown.rs`: the close, its backstop, the row ends and the daemon's one terminal exit.
- `signal_exit.rs`: the thread that catches INT and TERM and ends the daemon through the terminal's fire and then by the
  caught signal.
- `start_tests.rs`: tests of child creation against the fire, checked termination requests and partial births (an error
  or an unwind between creation and registration), on real processes. They are in-crate because they reach `Signal` and
  `contain`; the daemon-lifetime harness is a separate test binary and cannot.
- `start_tests_windows.rs`: the Windows half: a suspended start cannot be outwaited, job requests are checked, a killed daemon's contained tree ends with it and an outside capsule stand-in does not, and the suspended interval between creation and the job assignment is observed as ADR 0050's limit (a re-run of the test binary plays the daemon, inside a job of the case's own; `SOT_L2_NO_JOB_ASSIGNMENT` and `SOT_L2_PAUSE_ADOPT` are its two faults in `contain::assign`).
- `start_tests_macos.rs`: the macOS half: the recognition of a finished group before the reap, with injected observation faults.
- `startup.rs`: the start's decision from `held.json` and acting on it.
- `test_gates.rs`: the held points of the daemon-lifetime harness (feature `daemon-lifetime-faults`, `SOT_TEST_GATES`): a point waits until the case creates its file; an installed binary has none.

## Start here
`startup::begin` for what a start does; `shutdown::run` for the close and its order; `child_signal.rs` before a change
to how a child is owned.
