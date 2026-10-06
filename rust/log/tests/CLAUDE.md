# rust/log/tests: sot-log's integration and whole-process tests (capsule)

Each file or folder here is one test binary, run as `cargo test -p sot-log --test <name>`. They drive sot-log through its
public surface: the store and its recovery, the capsule runtime, the lane transports, the supervisor and the attach
client, several of them against a real `sot-capsule` process. Part of the capsule subsystem; charter: rust/log/CLAUDE.md.

## Files
- `attach_worker.rs`: `AttachWorker` against a real `sot-capsule supervise` and capsule: bounded ingress and a real multi-chunk checkpoint transfer; Linux and Windows (the multi-chunk test Linux only).
- `capsule/`: `capsule::run` driven with a test transport and fake or real producers (attach, group commit, early output end, shutdown paths; `unix_only.rs` and `windows_only.rs` hold the platform mechanism); Linux, macOS and Windows.
- `challenge_macos.rs`: the macOS identity challenge and the `SocketClient` connect path it authenticates, and the two credential-transition tests that pin a peer's account to the credential the kernel cached (their helper runs as root through `sudo -n`); macOS only.
- `challenge_unix.rs`: the Linux identity challenge (`authenticate_server`, `challenge`) and the `SocketClient` connect path it authenticates, each test process-isolated; Linux only.
- `claude_e2e.rs`: the real claude-sdk-helper and pinned SDK driving the Claude adapter against a fake Messages API, including the no-replay resume gate; Linux, and only with `SOT_HELPER_E2E=1`.
- `claude_rig.rs`: the Claude adapter (`claude::run`) against a scripted fake helper: turn table, WAL, redaction, terminal and successor closure, test-only unfenced mode; Linux.
- `connect_own.rs`: `connect_own`'s rule (ADR 0049, User isolation): a socket in a folder that is not private is refused, one in a private folder is accepted (Unix); a pipe another account serves is refused, one this account serves is accepted, and the pipe is opened at identification level (a server that impersonates the client gets `SecurityIdentification`) (Windows); and the two files that wrap their own dial still contain the rule's names (`own_socket(`, `connect_own(` or `own_pipe(`).
- `conpty.rs`: the owned ConPTY and job containment layer; Windows only.
- `e2e_pipe.rs`: a real capsule run over a real `PipeServer`, with watcher, driver and mgmt clients on one capsule; Windows only.
- `e2e_socket/`: the same end to end over a real `SocketServer` and `connect_voyage_socket` (`main.rs`), and the producer dying with its capsule through PDEATHSIG (`pdeathsig.rs`); Linux only.
- `fault_storage/`: native bounded-volume premises and storage-error boundary tests on Linux, macOS and Windows.
- `fault_kill.rs`: a randomized SIGKILL sweep of a real `sot-capsule` on a real PTY, then store recovery and chain continuation over many rounds on one voyage; Linux only.
- `fault_terminate.rs`: the portable terminate sweep with `sot-fault-writer`, killed mid-write, then store recovery; Unix and Windows.
- `fe_client/`: `FeAttachClient` against a real `sot-capsule supervise` and capsule: watcher attach, pen and resize order, `end_run`, reconnect (`pane.rs`), the headless client (`headless.rs`), the supervisor's word and the health window (`supervisor_word.rs`); Linux and Windows.
- `fixtures/`: committed bytes: the golden `.sotseg` segments, the pinned lane `.bin` files and the fake Messages API script.
- `golden.rs`: the v1 segment bytes pinned against the committed `.sotseg` fixtures; Unix and Windows.
- `macos_kernel_facts/`: the macOS kernel behaviours the lane rests on, one module per fact group (peer token, pty hangup, kqueue death watch, pty revoke); macOS only.
- `other_account.rs`: a client run as `sudo -n -u nobody` gets no byte from a `serve_own` listener, a client of this account does (ADR 0049, User isolation); Unix, skipped where passwordless sudo is not available.
- `pipe_win/`: `PipeServer` and the same-connection challenge over real pipes, process-isolated: connect, teardown, close, challenge modules; Windows only.
- `reconcile_matrix.rs`: every row of the startup reconciliation table (`reconcile`) entered by file surgery, then `verify_voyage`; Unix and Windows.
- `isolation_guards.rs`: the source guards of ADR 0049, User isolation, that a lint cannot make, over the production source (`sot_log::test_scan::production_sources()`): every accept of the Rust processes is a listed listener; the listeners the Julia children and Node helpers open in their source are listed; no browser opener outside `browser_open.rs`; no secret, token or address as a command-line argument; macOS's `LOCAL_PEERTOKEN` has one reader, and the source pin permits only pid/pidversion `.val` accesses in its private `AuditToken` module, pinning indices 5/7; the guard covers production source spellings, not arbitrary equivalent Rust, numeric socket-option calls or test-only token reads; every platform.
- `socket_unix/`: `SocketServer` and `SocketClient` over real Unix sockets, process-isolated: connect, teardown, close, client modules; Unix.
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
- A test file over 800 lines becomes `<name>/main.rs` plus subject modules under the same binary name; each module opens with `use super::*;`, so a helper two or more modules share stays in `main.rs`.
- A test that must run in a process of its own starts with `if !run_isolated("<name>") { return; }` (`sot_log::test_isolated`). `<name>` is its exact libtest name, with its module in a multi-module binary (`close::...` in `pipe_win/`); a name that runs no body fails. A test that re-runs its binary another way is listed in that module's pin with its own proof that the body ran.
- `support/` files are shared with `#[path = "support/<f>.rs"] mod <f>;` (`../support/` from a binary folder); each binary includes only what it uses.
- `fixtures/` holds committed bytes read by `include_bytes!` or through `CARGO_MANIFEST_DIR`; the four golden `.sotseg` files are also read by julia/sotlog/test/runtests.jl, so a fixture is never rewritten, only added.
- CI runs the Windows binaries by name in the `conpty-windows-2022` job of rust.yml: `conpty`, `capsule`, `pipe_win`, `e2e_pipe`, `supervisor`, `fe_client`.
- `claude_e2e` skips unless `SOT_HELPER_E2E=1`; rust.yml's `p2-e2e` job sets it against rust/log/claude-sdk-helper.
- scripts/tests/rc-gate.sh names `fe_client/supervisor_word::unresponsive_supervisor_expires_the_health_window` by path, so moving that test edits rc-gate.sh in the same commit.
- A lower-bound timing check takes its clock origin before the action that starts the product's timer.
