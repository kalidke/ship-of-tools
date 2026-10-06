# rust/frontend: the window binary `sot` (fe-ui)

The crate `sot-frontend` builds one binary, `sot`, the native window.
main builds the one-worker transport runtime and gives it to App; App::run owns the event loop and takes the runtime for shutdown_timeout(LEAVE_WRITE_WAIT) on both successful and error returns before the other App fields drop.
The code is split by subsystem into `src/ui/` (the window) and `src/net/` (its connections), and the few files beside
them each belong to one other subsystem, named below. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `Cargo.toml`: the crate and its one `[[bin]]`, `sot`; Windows-only dependencies for the foreground and taskbar calls.
- `build.rs`: on Windows, embeds the logo as sot.exe's icon resource (a no-op elsewhere).
- `queries/`: the Julia highlight query that `ui/preview/markdown/highlight.rs` embeds (fe-ui).
- `src/main.rs`: `main`, the process entry: tracing, the connection set, the transport runtime, then `ui::App` (fe-ui;
  charter rust/frontend/src/ui/CLAUDE.md).
- `src/browser_open.rs`: opens a served page in the OS browser through a one-use local redirect, so no page address is
  on a command line (pages; charter rust/backend/src/pages/CLAUDE.md).
- `src/cli.rs`: argv parsing, `Cli::parse` and the usage text (fe-ui; charter rust/frontend/src/ui/CLAUDE.md).
- `src/lease.rs`: the window's lease client, `Leases` and `Leaving` (lifecycle; charter
  rust/backend/src/lifecycle/CLAUDE.md).
- `src/lease_grant_tests.rs`: lease grant tests and the shared test-only private listener/handoff fixture.
- `src/lease_leave_tests.rs`: lease leave tests and the shared test-only recording leave peer and bounded log/finish helpers.
- `src/pages.rs`: the window's page proxy, loopback listeners that pipe each browser connection to the owning
  daemon's `proxy.connect` (pages; charter rust/backend/src/pages/CLAUDE.md).
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
- The window clears its own inherited standard handles first thing in `main` (`sot_log::host::winhandle::harden_own_stdio`), as
  the daemon does, and the browser opener's child gets null ones (`spawn_opener`); every other start the window makes
  set its three standard handles when read on 2026-10-05, so no child it starts holds its log files open
  (`the_window_clears_its_inherited_stdio_first`, `the_opener_hands_its_child_no_inherited_stdio`).
