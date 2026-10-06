# rust/backend/tests: sotd's integration suites (tests)

Each suite is its own test binary that drives a real `sotd` (and a real `sot-capsule` where rows run) over the real
wire, or runs a `sotd` subcommand as a subprocess. Isolation comes from support's `Env`, which gives every daemon its
own home, config, state, runtime and comm folders. The suites span subsystems, so this page names no charter.

## Files
- `active_frontend.rs`: server; which frontend is active, over the wire against a real `sotd`
- `admission.rs`: server; every connection starts with an accepted hello (every op of `sot_protocol::op` as a first frame is refused), and two OS accounts on one host are refused (ADR 0049 `## User isolation`)
- `agent_exec.rs`: agents; `sotd agent-exec` run as a plain subprocess, no daemon
- `ancestors.rs`: messaging; `sotd ancestors`, the process-ancestor listing comm-lib.sh counts agents with
- `comm_file.rs`: messaging; the inbox lock held by the daemon's filer and by the scripts' `sot_inbox_append`
- `comm_send.rs`: messaging; the staged `comm-send.sh` against a real `sotd`: `filed` only for a live handle, nothing appended for a gone one, and an idle row's daemon keeps it live, so a send is filed while that daemon is down
- `comm_wake.rs`: messaging; the comm wake tick on a real capsule row whose agent is a stub `claude`
- `control_session.rs`: server; a control session's replies pinned over the wire: unknown op, `monitor.*`, `pty.open` refusals, the off-loop ops, the evt skip and the refused hellos
- `daemon_boot.rs`: server; a first boot seeds the default row as the inert anchor, and the registry poll relays a state change; a spawned daemon inherits no `SOT_` variable the test did not set, and `sotd_command` is the only spawn site
- `hub_link.rs`: messaging; a hub `sotd` and a guest `sotd` joined by a stub `ssh`, broadcast filed on the guest
- `keystroke_latency.rs`: rows; keystroke timing against a private daemon, every test `#[ignore]`
- `live_socket.rs`: server; a second daemon on a live daemon's socket refuses and the first keeps answering
- `old_watcher.rs`: server; a pre-0.6.6 wake watcher's hello and `pty.input` in its library's exact form: the hello is refused with `protocol_mismatch`, then the connection ends with no other byte
- `ping_reaper.rs`: server; the reaper of half-open long-lived client roles, over the wire
- `preview_order.rs`: server; a `preview.get` written behind a `preview.set_scale` on one connection carries the new scale
- `relay_refresh.rs`: topology; `sotd topology refresh` on a scratch hub with a stand-in `systemctl`
- `shell_dial.rs`: topology; comm-lib's `sot_dial` (with and without its bound) and `sot_oneshot_request`, and the launch scripts' `sot_socket_open`, run by bash against the built `sotd`: a socket outside a private folder and another account's pipe are refused with nothing written, and `sot_dial` and `sot_socket_open` reach this account's; `sot_ssh_bridge` runs the far box's own `sotd stdio-bridge`; and `sot_bounded`, the bound each of the four timed comm calls runs under, through its one perl routine, on each platform's bash and perl, with a group that outlives its leader, a command that leaves its group and the grace period's length as tests of their own (ADR 0049 `## User isolation`)
- `status_integration.rs`: topology; `sotd status` against a real daemon the test starts and stops
- `stdio_bridge.rs`: topology; `sotd stdio-bridge [--host | --endpoint]` with real pipes and real processes
- `subcommand_help.rs`: server; every `sotd` subcommand's `--help` prints usage and dials nothing
- `topology_set.rs`: topology; `topology.set` and `topology.changed` over the wire; a hub daemon started from umask 022 creates its comm files owner-only
- `window_start.rs`: lifecycle; a daemon's start from `held.json`, resumed or ended rows
- `capsule_workspaces/`: rows; a real `sotd` and a real detached `sot-capsule` over a real local socket (`main.rs` plus modules)
- `lane_bridge/`: rows; a frontend attach client reaching a capsule row through a daemon and a Unix-socket relay that can be cut, blackholed and throttled
- `switch_latency/`: server; a slow request does not block a later cheap reply on one connection; its `dead_kernel` module is sidecars
- `window_lease/`: lifecycle; the close lifecycle's daemon half, one daemon per state root
- `support/`: the shared fixture: `mod.rs` (helpers, `poll_until`, `BOUND`, the attach wake flag), `sotd.rs` (`sotd_command`, also loaded alone by suites that need nothing else), `registry.rs` (`write_registry`), `env.rs` (`Env`), `procs.rs` (process spawning, the supervisor kill, the process count)
- `fixtures/`: data read by the backend's own unit tests (`comm/wake/screen_tests.rs`, `sidecars/monitor_tests.rs`) by path, not suites

## Start here
Read `support/mod.rs`, then `support/env.rs`, before writing a real-process suite. `live_socket.rs` is a small suite that
shows the shape.

## Rules
- A binary whose tests share one process takes its `SERIAL` before `Env::new`, which sets the process's `SOT_RUNTIME_DIR`
  (capsule_workspaces, comm_send, comm_wake, daemon_boot, lane_bridge, stdio_bridge, window_lease do; `Env::new` assumes it).
- Every `sotd` a suite starts comes from `sotd_command()` in `support/sotd.rs`, which drops every inherited `SOT_` variable;
  `sotd_exe` is private there (`daemon_boot.rs` checks that `CARGO_BIN_EXE_sotd` appears nowhere else).
- A suite writes a comm registry only through `support::write_registry`, which takes the registry lock as the daemon and the comm scripts do (`daemon_boot.rs` scans this folder for any other write).
- Every wait is bounded: `support::poll_until` and `BOUND`; a suite waits on a child process with `sot_log::test_isolated`'s `wait_within` or `drain(..).wait_within(..)` (`stdio_bridge.rs`, `shell_dial.rs`).
- A test that reads a stream it accepted makes it blocking (macOS keeps a non-blocking listener's `O_NONBLOCK` on an
  accepted socket; Linux does not) and bounds those reads by a deadline it owns, such as waiting for the reading
  thread's result, never by a read timeout on the accepted socket: Darwin refuses `SO_RCVTIMEO` there (EINVAL;
  rust/log/tests/macos_kernel_facts/peertoken.rs).
- A suite that re-runs a test binary is listed in `sot_log::test_isolated`'s pin with its own proof that the selected body ran (`comm_file.rs`, `window_lease/`). `comm_file.rs` drains and waits on its child with `sot_log::test_isolated::drain(..).wait_within(..)`.
- A suite never reaches the live box: `Env` points its daemon at its own folders (`comm_isolation_dirs`).
- A binary over 800 lines is `<name>/main.rs` plus modules, loading `#[path = "../support/mod.rs"] mod support;`.
- window_lease's `lease_holder_child` stays at its binary's root: `Window::open` runs it by exact name.
- scripts/tests/rc-gate.sh names binaries in `SPLIT` (capsule_workspaces, comm_wake, lane_bridge, fe_client) and some
  tests by module path in `SLOW_FIRST`; a renamed test changes there in the same commit.
- Two suites compile src files by `#[path]`: `support/mod.rs` (`row_scope_aim.rs`) and `comm_file.rs` (the inbox
  source). A move of either src file changes that line in the same commit.
