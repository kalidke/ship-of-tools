# rust/log/tests: sot-log's integration and whole-process tests (capsule)

Each file or folder here is one test binary, run as `cargo test -p sot-log --test <name>`. They drive sot-log through its
public surface: the store and its recovery, the capsule runtime, the lane transports, the supervisor and the attach
client, several of them against a real `sot-capsule` process. Part of the capsule subsystem; charter: rust/log/CLAUDE.md.

## Files
- `test_body_fixture.rs`: harmless real libtest bodies, including ignored and near-named controls, for the shell selected-test proofs.
- `attach_worker.rs`: `AttachWorker` against a real `sot-capsule supervise` and capsule: bounded ingress and a real multi-chunk checkpoint transfer; Linux and Windows (the multi-chunk test Linux only).
- `capsule/`: `capsule::run` driven with a test transport and fake or real producers (attach, group commit, early output end, shutdown paths; `unix_only.rs` and `windows_only.rs` hold the platform mechanism); Linux, macOS and Windows.
- `challenge_macos.rs`: the macOS identity challenge and the `SocketClient` connect path it authenticates, and the two credential-transition tests that pin a peer's account to the credential the kernel cached (their helper runs as root through `sudo -n`); macOS only.
- `challenge_unix.rs`: the Linux identity challenge (`authenticate_server`, `challenge`) and the `SocketClient` connect path it authenticates, each test process-isolated; Linux only.
- `claude_e2e.rs`: the real claude-sdk-helper and pinned SDK driving the Claude adapter against a fake Messages API, including the no-replay resume gate; Linux, and only with `SOT_HELPER_E2E=1`.
- `claude_rig.rs`: the Claude adapter (`claude::run`) against a scripted fake helper: turn table, WAL, redaction, terminal and successor closure, test-only unfenced mode; Linux.
- `connect_own.rs`: `connect_own`'s rule (ADR 0049, User isolation): a socket this account listens on is reached in any
  folder, one another account listens on is refused by the account recorded at `listen()`, a full backlog returns within
  `CONNECT_BOUND` plus the test's stated slack, a listener seen from a user namespace that maps no uid (`unshare -U`,
  Linux) is refused, and a missing socket is NotFound (Unix; the other account is `nobody`, through
  `sot_log::test_foreign`; the two cases with another account's listener need passwordless `sudo -n`, and skip without
  it except on CI); a pipe another account serves is refused, one this account serves is accepted, a busy one returns
  within `CONNECT_BOUND` plus the test's stated slack, and the pipe is opened at identification level (a server that
  impersonates the client gets `SecurityIdentification`) (Windows).
- `conpty.rs`: the owned ConPTY and job containment layer; Windows only.
- `e2e_pipe.rs`: a real capsule run over a real `PipeServer`, with watcher, driver and mgmt clients on one capsule; Windows only.
- `e2e_socket/`: the same end to end over a real `SocketServer` and `connect_voyage_socket` (`main.rs`), and the producer dying with its capsule through PDEATHSIG (`pdeathsig.rs`); Linux only.
- `fault_kill.rs`: a randomized SIGKILL sweep of a real `sot-capsule` on a real PTY, then store recovery and chain continuation over many rounds on one voyage; Linux only.
- `fault_storage/`: storage exhaustion on a real bounded volume (`main.rs`, `volume.rs`, `boundaries.rs`, `exits.rs` for a leg's exit code, and `scenario.rs` for supervisors that hold and resume (Linux and Windows)): a 256 MiB APFS image on macOS, a VHD on Windows, and on Linux the 64 MiB ext4 volume only rust.yml's "Test L3 storage exhaustion" step provides; Linux, macOS and Windows.
- `fault_terminate.rs`: the portable terminate sweep with `sot-fault-writer`, killed mid-write, then store recovery; Unix and Windows.
- `fe_client/`: `FeAttachClient` against a real `sot-capsule supervise` and capsule: watcher attach, pen and resize order, `end_run`, reconnect (`pane.rs`), the headless client (`headless.rs`), the supervisor's word and the health window (`supervisor_word.rs`); Linux and Windows.
- `fixtures/`: committed bytes: the golden `.sotseg` segments, the pinned lane `.bin` files and the fake Messages API script.
- `golden.rs`: the v1 segment bytes pinned against the committed `.sotseg` fixtures; Unix and Windows.
- `macos_kernel_facts/`: the macOS kernel behaviours the lane rests on, one module per fact group (peer token, pty hangup, kqueue death watch, pty revoke); macOS only.
- `other_account.rs`: a client run as `sudo -n -u nobody` gets no byte from a `serve_own` listener, a client of this account does (ADR 0049, User isolation); Unix, skipped where passwordless sudo is not available.
- `pipe_win/`: real pipe connect, teardown, close and challenge contracts plus reaping/shutdown regressions (`reaper.rs`); isolated selections require qualified identities and matching body-entry proof; Windows.
- `reconcile_matrix.rs`: every row of the startup reconciliation table (`reconcile`) entered by file surgery, then `verify_voyage`; Unix and Windows.
- `isolation_guards.rs`: remaining ADR 0049 source guards for browser-opener spellings and the macOS peer-token reader; Rust listener admission is proved at its native owners, not by an allowance catalog. REPL child arguments, selected environment and page-secret exclusion are observed in the backend REPL project_tests.rs; WGL listener selection and lifetime are observed in julia/repl/test/bonito/runtests.jl and the MathJax helper's tree in the backend's contract_tests.rs; this file retains only the unrelated browser-opener and macOS peer-token source guards.
- `socket_unix/`: real Unix connect, close, teardown and client contracts; named waits and captured diagnostics
  (`diagnostics.rs`), independent-reaping regressions (`reaper.rs`), the registration cutoff, a panicked server thread and `shutdown(2)` records
  (`shutdown.rs`), observed real factory birth/fallback flags (`cloexec.rs`), one bounded-read phase path (`read.rs`),
  and supervised native-account fixtures with retained failure causes (`privileged.rs`; the foreign-account case needs
  passwordless `sudo -n` and skips without it except on CI), process-isolated; Unix.
- `supervisor/`: the supervisor authority against a real `sot-capsule supervise` process: lifecycle, authority, spawn modules; Linux and Windows.
- `support/`: helpers shared by several binaries: `capsule_guard.rs` (a spawned `sot-capsule` no test can leave behind) and `transports.rs` (`NoopTransport` and `TestTransport`).
- `winhandle_windows.rs`: `winhandle::harden_own_stdio` clears handle inheritance; Windows only, alone in its binary because it mutates the process's real std handles.
- `wire/`: the `wire` frame codec through its public API, with arbitrary chunking and fuzzing (`main.rs`), and the supervisor and attach lane bytes pinned against committed fixtures (`pinned.rs`); every platform.

## Start here
- Store, segments or recovery: `golden`, `reconcile_matrix`, then `fault_terminate` (and `fault_kill` on Linux).
- The capsule runtime or a producer: `capsule`; with a real transport, `e2e_socket` (Linux) or `e2e_pipe` (Windows).
- The lanes and their wire format: `wire`, then `socket_unix` or `pipe_win`, and `challenge_unix` for identity.
- The supervisor: `supervisor`. The attach client: `fe_client`, and `attach_worker` for the transport half.
- The Claude adapter: `claude_rig`; `claude_e2e` only for the real helper.

## Rules
- Socket tests use named contexts and original deadlines; diagnostics captures available prerequisite history separately from each immediate timeout snapshot, whose busy/poisoned accounting is valid, and polls availability only for pre-expiry history/retention checks.
- A test file over 800 lines becomes `<name>/main.rs` plus subject modules under the same binary name; each module opens with `use super::*;`, so a helper two or more modules share stays in `main.rs`.
- Process-isolated tests use exact qualified names through `test_isolated`; direct fixtures finish owned-child checks before raising readiness/output errors. Readiness proofs require the observed role/pid start and exact selected failure; UTF-8 red requires independent child status/end checks and the specific stderr decoder cause. Wrong-failure controls exercise the same proof verifier; no source catalog proves these outcomes.
- `support/` files are shared with `#[path = "support/<f>.rs"] mod <f>;` (`../support/` from a binary folder); each binary includes only what it uses.
- `fixtures/` holds committed bytes read by `include_bytes!` or through `CARGO_MANIFEST_DIR`; the four golden `.sotseg` files are also read by julia/sotlog/test/runtests.jl, so a fixture is never rewritten, only added.
- CI runs the Windows binaries by name in the `conpty-windows-2022` job of rust.yml: `conpty`, `capsule`, `pipe_win`, `e2e_pipe`, `supervisor`, `fe_client`.
- `claude_e2e` skips unless `SOT_HELPER_E2E=1`; rust.yml's `p2-e2e` job sets it against rust/log/claude-sdk-helper.
- scripts/tests/rc-gate.sh names `fe_client/supervisor_word::unresponsive_supervisor_expires_the_health_window` by path, so moving that test edits rc-gate.sh in the same commit.
- A `fault_storage` volume test fails, never skips, when its volume cannot be made; it fills only its own volume, one volume at a time. On Linux it is `#[ignore]` and runs only in rust.yml's "Test L3 storage exhaustion" step, which sets `L3_HOSTED_VOLUME_ROOT` and passes `--include-ignored`.
- The `fault_storage` volume tests show the candidate's behavior on a real full volume, nothing more: the failing case before each storage fix is shown by unit tests with injected native errors, not on a real volume, because no host a builder may use has a bounded filesystem.
- A lower-bound timing check takes its clock origin before the action that starts the product's timer.
- Bounded socket reads distinguish deadline setup from read outcomes under the original context; capacity rejection still requires EOF. Native-account socket fixtures fail with retained status/output on missing entry or unavailable prerequisites; launcher completion alone proves no native body or denial.
- Socket and pipe reaper regressions observe real worker ownership, cancellation and event delivery; Unix factory tests observe descriptor flags at birth/publication and real flag-error cleanup. Role lists are runtime scenarios, not source-text inventory tests.
