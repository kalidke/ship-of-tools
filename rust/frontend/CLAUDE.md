# rust/frontend: the window binary `sot` (fe-ui)

The crate `sot-frontend` builds one binary, `sot`, the native window.
main builds the one-worker transport runtime and gives it to App; App::run owns the event loop and takes the runtime for shutdown_timeout(LEAVE_WRITE_WAIT) on both successful and error returns before the other App fields drop.
The code is split by subsystem into `src/ui/` (the window) and `src/net/` (its connections), and the few files beside
them each belong to one other subsystem, named below. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `Cargo.toml`: the crate and its one `[[bin]]`, `sot`; Windows-only dependencies for the foreground and taskbar calls.
- `Cargo.toml`'s window_close test target: feature-gated (`test-window-close`, which also enables the progress seam), harness=false main-thread native close proof, implemented by src/window_close_test.rs (its own root, which declares the product modules and calls `ui::run_native_window_close`) and ui/app/native_exit_tests.rs; it reuses the progress target's in-memory startup and bypasses ordinary configuration discovery.
- `Cargo.toml`'s window_minimized test target: feature-gated, harness=false main-thread native progress proof, implemented by src/main.rs and ui/app/tests.rs; ordinary startup is not run by that target.
- `Cargo.toml`'s pane_timing test target: feature-gated (`test-pane-timing`, which also enables the progress seam and the protocol's ssh fixture command), harness=false native relayed-attach timing, implemented by src/pane_timing_test.rs and ui/app/native_pane_tests.rs, native_pane_daemon_tests.rs and native_pane_route_tests.rs; the window dials `ssh:<hub>/<host>` over two real ssh logins (argument `ssh-relay`, Unix) or a `sotd stdio-bridge` stand-in (argument `stand-in`) to a private daemon.
- `build.rs`: on Windows, embeds the logo as sot.exe's icon resource (a no-op elsewhere).
- `queries/`: the Julia highlight query that `ui/preview/markdown/highlight.rs` embeds (fe-ui).
- `src/main.rs`: `main`, the process entry: tracing, the connection set, the transport runtime, then `ui::App` (fe-ui;
  charter rust/frontend/src/ui/CLAUDE.md).
- `src/window_close_test.rs`: the feature-gated, harness=false root of the window_close target: it declares the product modules and calls `ui::run_native_window_close`, copying no App, lease, render or startup logic.
- `src/pane_timing_test.rs`: the feature-gated, harness=false root of the pane_timing target: it declares the product modules and calls `ui::run_native_pane_timing`, copying no App, transport, lease, render or startup logic.
- `src/browser_open.rs`: opens a served page in the OS browser through a one-use local redirect, so no page address is
  on a command line (pages; charter rust/backend/src/pages/CLAUDE.md).
- `src/cli.rs`: argv parsing, `Cli::parse` and the usage text (fe-ui; charter rust/frontend/src/ui/CLAUDE.md).
- `src/lease.rs`: the window's lease client, `Leases` and `Leaving` (lifecycle; charter
  rust/backend/src/lifecycle/CLAUDE.md).
- `src/lease_grant_tests.rs`: lease grant tests and the shared test-only private listener/handoff fixture; its directory guard is created immediately after directory creation, before any fallible setup or binding, and behavioral tests cover bind-failure and successful-listener-drop cleanup.
- `src/lease_leave_tests.rs`: lease leave tests and the shared test-only recording leave peer and bounded log/finish helpers.
- `src/pages.rs`: the window's page proxy, loopback listeners that pipe each browser connection to the owning
  daemon's `proxy.connect`. The window's page proxy opens a dedicated SSH or generated-relay connection using the owning host's resolved control selection; handoff hello and proxy.connect share one write. (pages; charter rust/backend/src/pages/CLAUDE.md).
- `src/selfupdate.rs`: startup self-update staging and `--update-status` (distribution; charter scripts/CLAUDE.md).
- `src/relaunch.rs`: the relaunch sentinel, its watcher thread, and the Windows foreground handover (distribution;
  charter scripts/CLAUDE.md).
- `src/net/`: the window's connections to daemons (fe-net; charter rust/frontend/src/net/CLAUDE.md).
- `src/ui/`: the window itself (fe-ui; charter rust/frontend/src/ui/CLAUDE.md).

## Start here
`main` in src/main.rs: the runtime and the connection set, then `App`. For a change inside the window, go on to the
charter in src/ui/; for a connection or a request, to src/net/.

## Rules
- Exit codes 75 and 76 are a contract with the launcher scripts: the watcher in relaunch.rs (`spawn_watcher`) sets 75,
  or 76 when the sentinel's content is `converge`, and the window exits with it.
- `--ephemeral`, `--capture` and `--no-lease` never take a lease (`lease_exempt` in lease.rs).
- Only `sot_protocol::is_release_build()` self-updates (`guard` in selfupdate.rs); a dev build never stages anything.
- The state directory has one resolution rule, sot-log's, which the window calls directly.
- window_entry hardens the window's inherited Windows standard handles before its startup continuation. The Windows startup test observes the three inheritance flags and its first owned child's file handles; browser-opener stdio remains separately owned and unchanged.
- A Foreign lease outcome names a refused identity claim, not an absent backend or a proved different OS account; notice precedence is Undetermined, Unsupported, Foreign, then Unreached after granted/pending/exempt suppression.
- The hosted minimized-window check runs the actual winit application for ten minutes on Windows and macOS, confirms a minimized native window and reports event entry/progress/completion counts; unavailable GUI sessions are not passes.
- Minimized completion requires 12,000 actual fan-in events over the scheduled ten-minute workload, accepted-send timing/rate within the stated tolerance and matching actual dequeues. A short stalled-producer native control must fail the same workload validator before a queue-bound result is accepted.
- The native progress driver feeds a synthetic peer through transport's gated run_native_progress_transport entry, which executes the existing steady_loop and actual reply-to-fan-in send; its real App/State receiver supplies the dequeue evidence.
