# rust/backend: sotd, the daemon (backend)

sotd is one binary (Cargo.toml's one `[[bin]]`): the long-lived daemon that owns a computer's rows, the Julia children
behind them and the one control socket every client reaches it through. This folder is the crate root; its page covers
the crate folder and `src/`, and each folder under `src/` has its own page.

## Files
- `Cargo.toml`: the crate; one `[[bin]]` named `sotd` at `src/main.rs`
- `sidecars/`: `mathjax/`, the MathJax renderer (`render.mjs` and its npm lock) that `src/sidecars/mathjax.rs` runs
- `tests/`: the integration suites, each a real `sotd` over the real wire (own page)
- `src/main.rs`: boot: argv, umask, directory checks, the tee log, then `server::run`
- `src/clients.rs`: the roster of connected frontends (`Clients`, `ClientGuard`) and the `fe.*` and `version.query` ops that read it
- `src/clients_tests.rs`: unit tests of the roster
- `src/session.rs`: the revision counter and the bounded event ring a reconnecting client replays from (`Session::bump`)
- `src/paths.rs`: the platform helpers: state and socket paths, `resource_dir`, the private-directory checks
- `src/durable.rs`: the one fsynced write and delete for the records a later start acts on
- `src/update.rs`: the daemon's half of the updater: when to check, whom to notify, `update.check` and `update.apply`
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
- `main` answers `--help` first (`help_for`), then the early subcommand block, before any side effect: no log file or
  state directory exists yet, and `session-socket-path` and `--version` only print.
- Nothing is created before the umask and the directory checks: `apply_umask` (077), then `rows::store::check_config_dir`,
  then `paths::secure_private_dir` (refuses a state directory that is not private), then `open_private_log_file`.
- The log is `<state>/sotd.log` (mirrored to stdout by `TeeWriter`); nothing bounds or rotates it.
- An op's handler lives with the state it reads or writes; `server/dispatch.rs` (`dispatch`) only routes.
- Integration tests need `cargo build -p sot-log --bin sot-capsule` first.
- `sidecars/mathjax` is found at run time by `paths::resource_dir` and never moves.
