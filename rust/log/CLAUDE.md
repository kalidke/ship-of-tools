# rust/log: capsule (charter)

## Idea
A row's terminal lives in a capsule: one supervisor per state dir is the authority over its runs, one leg records the
agent's terminal into an append-only voyage that is read forever, and every viewer attaches through one client over typed
lanes. The crate is `sot-log`, the workspace's bottom crate, so it also carries the platform subsystem (`src/host/`,
`src/identity/`) that the daemon, frontend and protocol use for things that are not capsules.

## Owns
- The voyage store `<state_dir>/voyages/<id>/` with its `writer.lock`, segments and blobs (`src/store/`).
- The leg, `sot-capsule run`: the agent's kill domain, the terminal, the vt100 parser, the input WAL and the run-end
  marker (`src/capsule/`).
- The supervisor, `sot-capsule supervise`: `supervisor.lock`, `drawer.voyage`, `supervisor-journal/` and the legs
  (`src/supervisor/`).
- The three lockstep lanes (management, attach, supervisor) and their per-platform transports (`src/lane/`).
- The attach client every viewer uses, and the daemon's supervisor-lane calls (`src/attach_client/`).
- The dormant Claude SDK producer (`src/claude.rs`, `claude-sdk-helper/`): no product path runs it.

## Promises
- Written bytes are read forever: the format changes only through `codec_id`, `required_features` and the version seams.
  Only a provably torn tail is discarded (`Error::TornTail`, the one recoverable corruption); every other defect halts,
  and nothing is deleted.
- One writer per voyage (`writer.lock`, taken by `lock_writer`); one authority per state dir (`supervisor.lock`, taken
  by `lock_supervisor`).
- Output is published only after its fsync; attach is ground-gated (`writer_loop::output_path`), so a viewer joins
  only at a parser ground boundary.
- `sot-capsule supervise` exits 0 (clean), 69 (`EXIT_TERMINAL`) or 70 (`EXIT_CONTENDED`, the fence was already held).
- Storage exhaustion never ends a row: the leg exits 71, and its supervisor holds without charging its crash counter, probes the state root with a real durable write, and resumes the same voyage when a probe succeeds (`supervisor/storage/`).
- Only linux, macos and windows build: `host::durable::rename_noreplace_raw` has exactly those three arms.
- A reply on a local connection is trusted only after the challenge in `src/identity/`.

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: `sot-capsule supervise`,
`supervisor_client`, `FeAttachClient`, `rust/backend/src/rows/run/headless.rs`,
`rust/frontend/src/ui/agent_pane/attach.rs`, `drawer.voyage`, `writer.lock`, `Endpoint`, `DaemonLaneEndpoint`, `AnonymousJob`. Uses:
`DaemonLaneEndpoint`, `lane.connect`, `publish_noreplace`, `lock_writer`, `try_lock_daemon`, `preflight_volume`,
`owner_protected_pipe_descriptor`, `harden_own_stdio`, `boot_identity`, `process_created`, `IdentityExchange`, `storage_exhaustion`,
`Redial`, `STABLE`.

## Folders
- `src/store/`: the voyage store, its record codec, recovery and verifier.
- `src/capsule/`: the leg, one producer recorded into its voyage.
- `src/supervisor/`: the authority over a state dir's runs.
- `src/lane/`: transport contract, client seam and the platform bridge for the lanes.
- `src/attach_client/`: one client for every viewer of a capsule.
- `src/host/`: platform subsystem's folder: dirs, host name, durable publish, locks; the platform charter is
  `src/host/CLAUDE.md`.
- `src/identity/`: platform subsystem's peer challenge, covered by the same charter.
- `src/bin/`: the sot-log crate's binaries.
- `tests/`: integration and whole-process tests (its page maps each file).
- `claude-sdk-helper/`: the Node helper behind the dormant Claude producer.
- `../vt100/`: the project's fork of vt100-ctt, the terminal-state parser.
- `../../julia/sotlog/`: SotLog, the Julia reader of the golden segment fixtures.

## Files
- `Cargo.toml`: the crate manifest; the `test-support` feature is switched on for this crate's own tests, and brings
  `test_log`, `test_exec`, `test_scan`, `test_isolated` and, on Unix, `test_foreign` (the backend, frontend and updater
  turn it on in their dev-dependencies).
- `build.rs`: stamps the lane build id (`SOT_LOG_BUILD_SHA`) from the full git sha, or `SOT_BUILD_ID`.
- `claude-sdk-helper/`: the Node helper that drives one Claude Agent SDK session.
- `tests/`: integration and whole-process tests.
- `src/lib.rs`: the module tree, the crate's facades (`lock_writer`, `owner_protected_pipe_descriptor`) and `Error`/`Result`.
- `src/claude.rs`: the dormant Claude SDK producer.
- `src/secret.rs`: `redact` and `RedactingWriter`, the masking of page secrets in both binaries' logs.
- `src/test_exec.rs`: `write_executable`, the test-program writer (feature `test-support`); its Linux FIFO proof observes actual parent descriptors while the child writer is active, without a permission-spelling catalog.
- `src/test_isolated.rs`: shared exact-body isolation, scoped direct-fixture supervision and bounded child/output waits (feature `test-support`); readiness errors retain termination/entry checks, and byte-captured output renders invalid UTF-8 with explicit byte escapes. Wrapped fixtures retain launcher status and both streams, validate the native role/PID witness against exact entry, and share direct fixtures' wait/drain/finalization implementation.
- `src/test_log.rs`: timestamp-free `capture()` and formatting-preserving `install()` (feature `test-support`), with behavioral tests of the capture format, first-callsite routing, parallel isolation and guard restoration.
- `src/test_foreign.rs`: `ForeignListener`, a socket another OS account (`nobody`) listens on, for the User isolation
  tests, and `elevation_or_skip`, the one rule for a test that needs root: it skips only when `sudo -n true` fails (a
  failure on CI), and every later helper failure fails the test (feature `test-support`, Unix).
- `src/store/`: the voyage store.
- `src/capsule/`: the leg's runtime and producers.
- `src/supervisor/`: the supervisor, its journal, probe and authority.
- `src/lane/`: wire frames, transports and the attach protocol.
- `src/attach_client/`: the attach client and its worker.
- `src/identity/`: the peer challenge and identity exchange.
- `src/test_scan.rs`: the source scans' one walker, `rust_sources()` (every workspace member's `src/` and `tests/`), and its production view `production_sources()` with `without_test_modules`, and `enclosing` and `is_ident` (the `fn` or `struct` a match lies in, and the identifier test at its edges) (feature `test-support`).
- `src/host/`: per-machine facts and platform primitives.
- `src/bin/`: `sot-capsule`, `sot-log` and the three test-fixture binaries.

## Start here
Read `src/supervisor/mod.rs` for the process chain and exit codes, then `src/capsule/writer_loop/mod.rs` (`run`) for
the leg. For the record's format read `src/store/record.rs` and `src/store/segment.rs`; for a lane, `src/lane/wire/`.

## Rules
- Modules are `pub` where integration tests or other crates reach them: those see only pub items.
- `host` is a `pub` module; `lock_writer` and `owner_protected_pipe_descriptor` are also named at the crate root.
- A change to a wire tag, magic or limit, or to an exit code, is an interface change with other processes and versions.
- A fixture selects its executable on the child command. T2's actual SSH fixtures compare the parent PATH and SHELL before, during and after execution; no lexical allowlist proves environment preservation.
- Every subscriber a test installs goes through `test_log`: `capture()` reads this thread's events without formatter timestamps or colour; `install()` preserves the supplied subscriber's formatting and timer. Behavioral tests exercise first-callsite routing, parallel captures and guard restoration; new callers use this owner. The two binaries' `main` functions install production subscribers.
- Test-program writers use `test_exec::write_executable`; its separate Unix writer leaves no writable program descriptor in the test process. The retained real descriptor test proves this owner property; review checks callers, without a source-spelling inventory.
- Named test reruns use `test_isolated`; direct fixtures retain child wait/status, termination, entry and separate output observations before raising failures. Regression proofs require observed prerequisites and the selected failure's exact cause; wrong-cause controls use the same verifier. Duration/deadline APIs share one wait implementation; byte output preserves valid text and escapes invalid UTF-8. Wrapped fixtures retain launcher status and both streams, validate the native role/PID witness against exact entry, and share direct fixtures' wait/drain/finalization implementation; ordinary direct fixtures still validate their spawned child PID.
- Every Rust source-text scan reads through `test_scan`, and none cuts a file at its first `#[cfg(test)]` (`test_scan::tests::no_scan_cuts_a_file_at_its_first_cfg_test`).
- Helper binaries `sot-pty-helper`, `sot-conpty-helper` and `sot-fault-writer` are test fixtures and never ship.
- Another crate can reach the processes this crate starts (the capsule's agent, the supervisor's legs, the Claude
  adapter's helper) through its public items, as read on 2026-10-05: `capsule::run`, `capsule::producer::Producer::spawn`,
  `capsule::producer::conpty::ConptySpawn::spawn`, `supervisor::supervise`,
  `supervisor::probe::classify::probe_owned_spawn`, `supervisor::probe::ProbeOps::spawn` and `claude::run`.
  `rust/clippy.toml` lists none of them, so the lint does not see a call from another crate; today no other crate calls
  them but `src/bin/sot-capsule.rs`, which reaches them through `capsule::run`, `supervisor::supervise` and
  `claude::run`.
