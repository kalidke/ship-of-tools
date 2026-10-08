# rust/log/src/bin: the sot-log crate's binaries (capsule)

`sot-capsule` is the capsule's one shipped binary: it runs a producer on a real terminal and records its voyage
(`run`), supervises one (`supervise`), ends or resets a run (`endrun`, `reset`), prints its lane build id (`build-id`)
and, on Linux only, runs the Claude helper (`claude`). `sot-log` is the developer CLI for `verify`. The other three are
test fixtures that the suites in `rust/log/tests` launch. Part of capsule; charter: rust/log/CLAUDE.md.

## Files
- `sot-capsule.rs`: the shipped capsule binary; `main` dispatches the subcommands to the `cmd_*` functions
- `sot-log.rs`: developer CLI, `sot-log verify <voyage_root> <voyage_id> [--allow-open-tip]`
- `sot-pty-helper.rs`: Unix test fixture, a pty-driven helper with flood, script and drip modes
- `sot-conpty-helper.rs`: Windows twin of the pty helper, same modes and same bytes
- `sot-fault-writer.rs`: cross-platform voyage writer that the fault sweep kills at a random moment
- `support/`: `helper_common.rs`, the flood pattern and script block both pty helpers share (no page of its own)

## Start here
`sot-capsule.rs`: `main` and its `cmd_*` functions, for any argv or exit-code change.

## Rules
- Only `sot-capsule` ships: release.yml builds it with `sotd`, and install.sh copies `sot`, `sotd` and `sot-capsule`.
  The other four binaries are never packaged.
- `sot-capsule run` exits 71 (`sot_log::capsule::EXIT_LEG_STORAGE_FULL`) only when its run failed with storage exhaustion (`storage_exhaustion`); otherwise `leg_exit_code` maps `ExitStatus::Code(c)` to c (a producer's own 71 becomes 1; the voyage still records 71), `Signal(n)` to 128+n, and any other run error to 1.
  `supervise` exits with the code `sot_log::supervisor::supervise` returns (0, 69 `EXIT_TERMINAL` or 70
  `EXIT_CONTENDED`), or 2 on a usage error. These codes are an interface with the daemon and the supervisor.
- The two pty helpers emit identical bytes because both `#[path]`-include `support/helper_common.rs`.
- The helpers and the fault writer are reached by `rust/log/tests` through `CARGO_BIN_EXE_*` only.
