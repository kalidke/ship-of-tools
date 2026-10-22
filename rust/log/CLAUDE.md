# rust/log: capsule (charter)

## Idea
A row's terminal lives in a capsule: one supervisor per state dir is the authority over its runs, one leg runs the
agent on a real terminal and keeps its latest screen, and every viewer attaches through one client over typed lanes.
No append-only record of the terminal is kept (decision D2, pass 6a). The crate is `sot-log`; it rests on
`sot-platform` (`../platform/CLAUDE.md`), which holds the machine facts, durable publication, locks and the peer
challenge's OS steps, and re-exports none of it. The capsule's contract is written in pass 6d; this page says what is
true after the voyage's removal.

## Owns
- The leg's folder `<state_dir>/voyages/<id>/`: its `writer.lock` fence, `leg` (the number of the leg that last started
  there) and `ended` (how the leg ended: whether an end was asked for, the producer's uptime and exit; `src/capsule/fence.rs`,
  `src/capsule/ended.rs`). The folder holds no segments and no blobs.
- The leg, `sot-capsule run`: the agent's kill domain, the terminal, the vt100 parser and the screen file
  `<state_dir>/screen` (`src/capsule/`).
- The supervisor, `sot-capsule supervise`: `supervisor.lock`, `drawer.voyage`, `supervisor-journal/` and the legs
  (`src/supervisor/`).
- The three lockstep lanes (management, attach, supervisor) and their per-platform transports (`src/lane/`).
- The attach client every viewer uses, and the daemon's supervisor-lane calls (`src/attach_client/`).

## Promises
- The leg keeps `<state_dir>/screen`: the vt100 fork's checkpoint (format 2), handed every 5 s while the screen
  changed (and at the agent's end) to one writer thread, which writes it through platform's `write_durable`. The slot
  between them keeps only the newest snapshot, so a stalled or failing disk never reaches the output loop or the
  management lane; a failed write is noted once per failing streak and the next pace hands over a fresh snapshot. The
  leg's end waits for the writer at most 2 s. A leg starts its parser from the file, with a
  `[restarted <UTC time>]` line below the retained content (on Windows both are scrolled into the history, because the
  ConPTY host repaints the visible screen at start); a file that does not restore is removed and the screen starts
  empty. Limits: the file can be 5 s old, a failing disk can leave it old for good, a supervisor `Stop` leaves a live
  leg running with no flush, and scrollback in it is at most `CAPSULE_SCROLLBACK_ROWS` (200).
- The leg writes `ended` before it acknowledges an end request (`requested`, durably) and again whole when the
  producer is gone; a leg that crashed leaves no finished file, and a supervisor reads it as unstable. The startup
  check does not respawn a leg over a `requested` file; a leg ended some other way is respawned.
- Input is written at most once: `input_recorded` means the bytes were written to the pty's master. The client reports
  an input whose acknowledgement was lost as delivery unknown and never sends it again.
- One leg per voyage folder (`writer.lock`, taken by `fence::take_fence`); one authority per state dir
  (`supervisor.lock`, taken by `lock_supervisor`).
- Attach is ground-gated (`writer_loop::output_path::ground_check`), so a viewer joins only at a parser ground
  boundary, after every byte the parser has seen was published.
- `sot-capsule supervise` exits 0 (clean), 69 (`EXIT_TERMINAL`) or 70 (`EXIT_CONTENDED`, the fence was already held).
  There is no storage hold and no exit 71: a failed write of the supervisor's own journal or pointer ends the operation
  as any other failure does.
- Only linux, macos and windows build.
- A reply on a local connection is trusted only after the challenge in platform (`../platform/src/identity/`); the
  lanes' own request and reply bytes are `src/lane/exchange.rs`.

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: `sot-capsule supervise`,
`supervisor_client`, `FeAttachClient`, `rust/backend/src/rows/run/headless.rs`,
`rust/frontend/src/ui/agent_pane/attach.rs`, `drawer.voyage`, `writer.lock`, `ended`, `screen`, `Endpoint`, `DaemonLaneEndpoint`, `AnonymousJob`. Uses:
`DaemonLaneEndpoint`, `lane.connect`, `publish_noreplace`, `lock_writer`, `try_lock_daemon`, `preflight_volume`,
`owner_protected_pipe_descriptor`, `harden_own_stdio`, `inherit_none`, `boot_identity`, `process_created`, `IdentityExchange`, `write_durable`, `remove_durable`, `storage_exhaustion`,
`Redial`, `STABLE`.

## Folders
- `src/capsule/`: the leg, one producer under a live parser, its screen file and its ending.
- `src/supervisor/`: the authority over a state dir's runs.
- `src/lane/`: transport contract, client seam and the platform bridge for the lanes.
- `src/attach_client/`: one client for every viewer of a capsule.
- `../platform/`: the platform crate, whose charter is `../platform/CLAUDE.md`.
- `src/bin/`: the sot-log crate's binaries (`sot-capsule` and the two helpers).
- `tests/`: integration and whole-process tests (its page maps each file).
- `../vt100/`: the project's fork of vt100-ctt, the terminal-state parser.

## Files
- `Cargo.toml`: the crate manifest; the `test-support` feature is switched on for this crate's own tests, and brings
  `test_log`, `test_copy`, `test_scan` and, on Unix, `test_foreign`, and turns on platform's `test_isolated` and `test_wait` (the
  backend, frontend and updater turn it on in their dev-dependencies, and platform's own beside it).
- `build.rs`: stamps the lane build id (`SOT_LOG_BUILD_SHA`) from the full git sha, or `SOT_BUILD_ID`.
- `tests/`: integration and whole-process tests.
- `src/lib.rs`: the module tree, the crate's facades (`lock_writer`, `owner_protected_pipe_descriptor`) and `Error`/`Result`.
- `src/secret.rs`: `redact` and `RedactingWriter`, the masking of page secrets in both binaries' logs.
- `src/test_barrier.rs`: `hold`, a named phase barrier a real process can be held at (Unix, feature `native-barrier`, which no installed build enables; inert unless `SOT_TEST_BARRIER_DIR` names a folder).
- `src/test_copy.rs`: `copy_release_folder`, the one copy of a project folder for a test, leaving out every Julia manifest (feature `test-support`).
- `src/test_isolated.rs`: shared exact-body isolation, scoped direct-fixture supervision and bounded child/output waits (feature `test-support`); `Draining::wait_and_read` returns child and stream observations without panicking; `try_wait_within` returns a checked tuple or failure text so a caller can scrub it before reporting, and `wait_within` is its panicking adapter. Both checked forms retain the child's status or wait failure, both stream failures and each stream's last 8 KiB. Readiness errors retain termination/entry checks, and byte-captured output renders invalid UTF-8 with explicit byte escapes. Wrapped fixtures retain launcher status and both streams, validate the native role/PID witness against exact entry, and share direct fixtures' wait/drain/finalization implementation.
- `src/test_wait.rs`: the bounded form of a test's wait (feature `test-support`): `run_within` runs a child to its end and returns `Ran` (its status and both streams), whose `Display` is the account every failure about it carries, each stream cut to its last 8 KiB by `tail`, which `test_isolated`'s drained waits share; `try_run_within` is its form that never panics, for a caller that takes the failure as a value (a cleanup, a probe whose failure a skip names); `within` bounds any other blocking wait on a thread of its own.
- `src/test_log.rs`: timestamp-free `capture()` and formatting-preserving `install()` (feature `test-support`), with behavioral tests of the capture format, first-callsite routing, parallel isolation and guard restoration.
- `src/test_foreign.rs`: `ForeignListener`, a socket another OS account (`nobody`) listens on, for the User isolation
  tests, and `elevation_or_skip`, the one rule for a test that needs root: it skips only when `sudo -n true` fails (a
  failure on CI), and every later helper failure fails the test (feature `test-support`, Unix).
- `src/capsule/`: the leg's runtime, its screen and ended files, and the producers.
- `src/supervisor/`: the supervisor, its journal, probe and authority.
- `src/lane/`: wire frames, transports and the attach protocol.
- `src/attach_client/`: the attach client and its worker.
- `src/lane/exchange.rs`, `src/lane/connect_own.rs`: the lanes' identity exchanges, and the connect that asks platform's account check.
- `src/storage.rs`: `storage_exhaustion`, the capsule's reading of platform's storage table.
- `src/test_scan.rs`: the source scans' one walker, `rust_sources()` (every workspace member's `src/` and `tests/`), and its production view `production_sources()` with `without_test_modules`, and `enclosing` and `is_ident` (the `fn` or `struct` a match lies in, and the identifier test at its edges) (feature `test-support`).
- `src/bin/`: `sot-capsule` and the two test-fixture helpers.

## Start here
Read `src/supervisor/mod.rs` for the process chain and exit codes, then `src/capsule/writer_loop/mod.rs` (`run`) for
the leg, `src/capsule/screen.rs` and `src/capsule/ended.rs` for the two files it leaves; for a lane, `src/lane/wire/`.

## Rules
- Modules are `pub` where integration tests or other crates reach them: those see only pub items.
- `host` is a `pub` module; `lock_writer` and `owner_protected_pipe_descriptor` are also named at the crate root.
- A change to a wire tag, magic or limit, or to an exit code, is an interface change with other processes and versions.
- A fixture selects its executable on the child command. T2's actual SSH fixtures compare the parent PATH and SHELL before, during and after execution; no lexical allowlist proves environment preservation.
- Every subscriber a test installs goes through `test_log`: `capture()` reads this thread's events without formatter timestamps or colour; `install()` preserves the supplied subscriber's formatting and timer. Behavioral tests exercise first-callsite routing, parallel captures and guard restoration; new callers use this owner. The two binaries' `main` functions install production subscribers.
- Test-program writers use `sot_platform::test_exec::write_executable` (platform's, feature `test-support`; its Linux FIFO proof observes actual parent descriptors while the child writer is active), a copy of a built program included (`write_executable(&to, std::fs::read(&from)?)`, never `std::fs::copy`, whose writer is the test process); its separate Unix writer leaves no writable program descriptor in the test process. The retained real descriptor test proves this owner property; `std::fs::copy` is in `rust/clippy-tests/clippy.toml`'s disallowed methods, so a copy of a program is a compile error in the clippy all-targets run, and a data copy carries `#[allow(clippy::disallowed_methods, reason = ...)]` on its statement.
- Named test reruns use `test_isolated`; direct fixtures retain child wait/status, termination, entry and separate output observations before raising failures. Regression proofs require observed prerequisites and the selected failure's exact cause; wrong-cause controls use the same verifier. Duration/deadline APIs share one wait implementation; byte output preserves valid text and escapes invalid UTF-8. Wrapped fixtures retain launcher status and both streams, validate the native role/PID witness against exact entry, and share direct fixtures' wait/drain/finalization implementation; ordinary direct fixtures still validate their spawned child PID.
- Every Rust source-text scan reads through `test_scan`, and none cuts a file at its first `#[cfg(test)]` (`test_scan::tests::no_scan_cuts_a_file_at_its_first_cfg_test`).
- Helper binaries `sot-pty-helper` and `sot-conpty-helper` are test fixtures and never ship.
- Another crate can reach the processes this crate starts (the capsule's agent, the supervisor's legs) through its public items, as read on 2026-10-05: `capsule::run`, `capsule::producer::Producer::spawn`,
  `capsule::producer::conpty::ConptySpawn::spawn`, `supervisor::supervise`,
  `supervisor::probe::classify::probe_owned_spawn`, `supervisor::probe::ProbeOps::spawn` and, on
  Unix, `host::process_tree::Launch::begin` (the native launcher). `rust/clippy.toml` lists none of them, so the lint
  does not see a call from another crate; today no other crate calls them but `src/bin/sot-capsule.rs`, which reaches
  them through `capsule::run` and `supervisor::supervise`, and the daemon-lifetime harness
  (`rust/backend/tests/daemon_lifetime`), which calls `Launch::begin`.
