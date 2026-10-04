# rust/backend/tests: sotd's integration suites (tests)

Each suite is its own test binary that drives a real `sotd` (and a real `sot-capsule` where rows run) over the real
wire, or runs a `sotd` subcommand as a subprocess. Isolation comes from support's `Env`, which gives every daemon its
own home, config, state, runtime and comm folders. The suites span subsystems, so this page names no charter.

## Files
- `active_frontend.rs`: server; which frontend is active, over the wire against a real `sotd`
- `agent_exec.rs`: agents; `sotd agent-exec` run as a plain subprocess, no daemon
- `ancestors.rs`: messaging; `sotd ancestors`, the process-ancestor listing comm-lib.sh counts agents with
- `comm_file.rs`: messaging; the inbox lock held by the daemon's filer and by the scripts' `sot_inbox_append`
- `comm_wake.rs`: messaging; the comm wake tick on a real capsule row whose agent is a stub `claude`
- `control_session.rs`: server; a control session's replies pinned over the wire: unknown op, `monitor.*`, `pty.open` refusals, the off-loop ops, the evt skip and the protocol-gated roster
- `daemon_boot.rs`: server; a first boot seeds the default row as the inert anchor, and the registry poll relays a state change
- `hub_link.rs`: messaging; a hub `sotd` and a guest `sotd` joined by a stub `ssh`, broadcast filed on the guest
- `keystroke_latency.rs`: rows; keystroke timing against a private daemon, every test `#[ignore]`
- `live_socket.rs`: server; a second daemon on a live daemon's socket refuses and the first keeps answering
- `ping_reaper.rs`: server; the reaper of half-open long-lived client roles, over the wire
- `relay_refresh.rs`: topology; `sotd topology refresh` on a scratch hub with a stand-in `systemctl`
- `status_integration.rs`: topology; `sotd status` against a real daemon the test starts and stops
- `stdio_bridge.rs`: topology; `sotd stdio-bridge [--host]` with real pipes and real processes
- `subcommand_help.rs`: server; every `sotd` subcommand's `--help` prints usage and dials nothing
- `topology_set.rs`: topology; `topology.set` and `topology.changed` over the wire
- `window_start.rs`: lifecycle; a daemon's start from `held.json`, resumed or ended rows
- `capsule_workspaces/`: rows; a real `sotd` and a real detached `sot-capsule` over a real local socket (`main.rs` plus modules)
- `lane_bridge/`: rows; a frontend attach client reaching a capsule row through a daemon and a TCP-to-Unix relay
- `switch_latency/`: server; a slow request does not block a later cheap reply on one connection; its `dead_kernel` module is sidecars
- `window_lease/`: lifecycle; the close lifecycle's daemon half, one daemon per state root
- `support/`: the shared fixture: `mod.rs` (helpers, `poll_until`, `BOUND`, the attach wake flag), `env.rs` (`Env`), `procs.rs` (process spawning, the supervisor kill, the process count)
- `fixtures/`: data read by the backend's own unit tests (`comm/wake/screen_tests.rs`, `sidecars/monitor_tests.rs`) by path, not suites

## Start here
Read `support/mod.rs`, then `support/env.rs`, before writing a real-process suite. `live_socket.rs` is a small suite that
shows the shape.

## Rules
- A binary whose tests share one process takes its `SERIAL` before `Env::new`, which sets the process's `SOT_RUNTIME_DIR`
  (capsule_workspaces, comm_wake, lane_bridge, stdio_bridge, window_lease do; `Env::new` assumes it).
- Every wait is bounded: `support::poll_until` and `BOUND`.
- A suite never reaches the live box: `Env` points its daemon at its own folders (`comm_isolation_dirs`).
- A binary over 800 lines is `<name>/main.rs` plus modules, loading `#[path = "../support/mod.rs"] mod support;`.
- window_lease's `lease_holder_child` stays at its binary's root: `Window::open` runs it by exact name.
- scripts/tests/rc-gate.sh names binaries in `SPLIT` (capsule_workspaces, comm_wake, lane_bridge, fe_client) and some
  tests by module path in `SLOW_FIRST`; a renamed test changes there in the same commit.
- Two suites compile src files by `#[path]`: `support/mod.rs` (`row_scope_aim.rs`) and `comm_file.rs` (the inbox
  source). A move of either src file changes that line in the same commit.
