# rust/backend: sotd, the daemon (backend)

sotd is one binary (Cargo.toml's one `[[bin]]`): the long-lived daemon that owns a computer's rows, the Julia children
behind them and the one control socket every client reaches it through. This folder is the crate root; its page covers
the crate folder and `src/`, and each folder under `src/` has its own page.

## Files
- `Cargo.toml`: the crate; one `[[bin]]` named `sotd` at `src/main.rs`, and the test-only feature `daemon-lifetime-faults` (phase barriers for `tests/daemon_lifetime`; no installed build has it)
- `sidecars/`: `mathjax/`, the MathJax renderer (`render.mjs` and its npm lock) that `src/sidecars/mathjax.rs` runs
- `tests/`: the integration suites, each a real `sotd` over the real wire (own page)
- `src/main.rs`: subcommand routing, offline trust declaration, then daemon boot: umask, directory checks, the tee log and `server::run`.
- `src/clients.rs`: the connected-frontend roster and fe.* and version.query handlers; fe.command.send logs optional forwarded workspace/path plus target and delivery count.
- `src/clients_show.rs`: which window a command goes to: a preview to every window, anything else to the window in use
- `src/clients_window.rs`: a window's state across its connections (the REPL runs it numbered, its presence, the owner
  of its pages), so a window that redials keeps it; a window is its hello's host and `instance`
- `src/clients_relay_tests.rs`: the relay log of fe.command.send: forwarded workspace and path, target and delivery count
- `src/clients_tests.rs`: unit tests of the roster
- `src/session.rs`: the revision counter and the bounded event ring a reconnecting client replays from (`Session::bump`)
- `src/paths.rs`: the platform helpers: state and socket paths, `resource_dir`, the private-directory checks
- `src/durable.rs`: the one fsynced write and delete for the records a later start acts on
- `src/update.rs`: the daemon's half of the updater: when to check, whom to notify, `UpdaterSpawner` over the child signal, `update.check` and `update.apply`, and the stage-prepare-arm both run. It follows the install on disk, not its own boot version (`check_basis`), arms only above it, never into a dev build, and not from a host the topology gives neither `daemon` nor `frontend` (`arm_refusal`, `host_runs_no_sotd`); `update.apply` refuses a pointer that no longer fits (`apply_refusal`).
- `src/update/`: `sotd update stage <folder> --prefix <prefix>`, the operator's stage-prepare-arm of a packed folder, and `sotd update pointer --prefix <prefix> --target <target>`, the offline query `sot-apply.sh` and `sot-apply.ps1` ask (exit 3 refuses the pointer).
- `src/agents/`: accounts, folder trust, the awareness env and the launch recipe (agents)
- `src/comm/`: the daemon's half of messaging: delivery, the registry and the wake (messaging)
- `src/files/`: workspace file reads, writes, previews, confinement and the watcher (files)
- `src/lifecycle/`: window leases, the start plan, the close sequence and the child signal (lifecycle)
- `src/pages/`: the loopback page servers and the proxy for a remote window (pages)
- `src/rows/`: the row registry, the row toml store, and the supervisor's spawn and run (rows)
- `src/server/`: the listener, one task per connection and the op table (server)
- `src/sidecars/`: the Julia kernel, REPL, Pluto, MathJax and monitor children (sidecars)
- `src/topology/`: what the daemon derives from `hosts.toml`, and the `topology` and `status` commands (topology)

## Start here
`main` in `src/main.rs` for boot; `src/server/` for a connection; the owning folder for an op.

## Rules
- `main` is synchronous and builds its runtime itself (`runtime`) after the serving prologue, so that what must exist before
  any thread can, in this order: on Linux the check that the process has one thread
  (`lifecycle::daemon_children::guard::require_one_thread`), which refuses the boot otherwise; on Unix the durable parent
  (`rows::spawn::durable::proxy::start_before_runtime`), once, never restarted; then on Linux the lifetime guard
  (`lifecycle::daemon_children::guard::install`), which forks the daemon as the child of a process that kills what the
  daemon started when it ends; then the thread that catches INT and TERM (`lifecycle::signal_exit::install`), which refuses the
  boot if it cannot be installed. The runtime, the relay refresh and every other thread come after these. `main` blocks on
  `server::run` through `complete_main`, which turns the main future's result into its exit code (Ok 0, an error 1, a
  panic 101) while the runtime still exists, and every exit of the serving daemon takes `lifecycle::shutdown::exit`.
- `main` answers help before any side effect. Query/bridge subcommands keep their existing contracts; `trust declare`
  is an offline settings mutation and returns before daemon initialization.
- Daemon boot creates nothing before the umask and directory checks. Offline trust declaration applies the private umask
  before its own writes and creates no daemon log, state, row or listener.
- The log is `<state>/sotd.log` (mirrored to stdout by `TeeWriter` unless stdout is a regular file, a launcher's per-start log: `stdout_is_file`, decided once at start). Every writer, the capsule supervisors included, holds its own handle on that file; after each daemon write that leaves it over 64 MiB (its size read from the daemon's handle, which counts every writer's appends), `roll_log` copies the last 64 MiB to `sotd.log.1` (replacing any older) and truncates it in place, so no writer's lines go to an unlinked file. A line another writer appends between the copy's last read and the truncate is lost (microseconds on a local disk, seconds on a network one). A roll that fails is reported once and retried after another 64 MiB of growth. A launcher's per-start file then holds only stderr, so the stdout copy that remains (a terminal, a pipe, the journal) is not the launcher's to bound.
- An op's handler lives with the state it reads or writes; `server/dispatch.rs` (`dispatch`) only routes.
- Integration tests need `cargo build -p sot-log --bin sot-capsule` first.
- `sidecars/mathjax` is found at run time by `paths::resource_dir` and never moves.
