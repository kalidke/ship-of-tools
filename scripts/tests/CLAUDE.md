# scripts/tests/: suites for install, apply and launch (distribution)

Hermetic suites for the scripts in scripts/, and the local candidate gate. Each suite builds its prefix, home or stub
`sotd` under a temp folder and touches no live daemon. Part of distribution; charter: scripts/CLAUDE.md.

## Files
- `installer-state.sh`: install.sh's decisions, the rendered unit and wrapper, `sot_daemon_ensure`, the log pruner,
  and the pinned bounds and copies. Runs in the `rust.yml` step "Test installer state (bash)" (ubuntu leg) and in
  `rc-gate.sh`.
- `installer-apply.sh`: `sot-apply.sh` apply and rollback, the one-copy helper and the network refusal. Runs in the
  `rust.yml` step "Test installer apply (bash)" (ubuntu leg) and in `rc-gate.sh`.
- `installer-support.sh`: the setup both installer suites source: install.sh and lib/sot-daemon.sh, `check`,
  `starts_with`, `case_start`, the sandboxed tool dir (`mk_tools`) and the recording stubs (`mk_stubs`).
- `rc-gate.sh`: the local candidate gate: the Rust workspace tests, doc tests, windows-gnu and darwin cross checks,
  every Julia suite and the shell suites, as concurrent jobs under one cap. Linux only, run by hand.
- `test-install-layout.ps1`: `Test-SotPinnedCheckout`, `Get-SotLauncherTarget`, `Get-SotLauncherCodeId` and
  `Set-SotFolderTrust` (scripts/sot-install-layout.ps1). Runs in the `rust.yml` step "Test install layout
  (pinned-checkout predicate)".
- `test-local-daemon.ps1`: scripts/sot-local-daemon.ps1 start, stop and wait behaviour (sections 0-8 and 12-15: the
  refusal, the pipe name, a late bind, `-Stop`, log retention, `Get-StopWaitMs`). Runs in the `rust.yml` step "Test
  local daemon launcher".
- `test-launcher-leases.ps1`: launch-sot.ps1's ensure and lease order in the supervisor loop and the converge lease
  (sections 9-11 and 16 of the old suite), read as syntax trees and run against the fake `sotd.exe`. Runs in the
  `rust.yml` step "Test launcher leases".
- `test-local-daemon-fake.ps1`: dot-sourced by both local-daemon suites: compiles the fake `sotd.exe` and defines
  `Clear-FakeEnv`, `New-FakePrefix` and `Stop-FakeOn`.
- `test-local-daemon-support.ps1`: dot-sourced by both local-daemon suites: `Check`, the fixture and pipe helpers, the
  test root and `Complete-LocalDaemonTest`, their cleanup.
- `test-sot-apply.ps1`: scripts/sot-apply.ps1 against a synthetic staged update: apply, damaged stage, rollback,
  already applied, wrong target, lock held. Runs in the `rust.yml` step "Test sot-apply.ps1".
- `test-topology-plan.ps1`: `Get-SotTopologyPlan` and `Invoke-SotTopologySync` (scripts/sot-hosts.ps1) against a fake
  `sotd`. Runs in the `rust.yml` step "Test topology plan".
- `test-topology-plan.sh`: `sot_topology_plan` (scripts/lib/sot-hosts.sh) against a fake `sotd`. Runs in the `rust.yml` step
  "Test topology plan (bash)" (ubuntu leg) and in `rc-gate.sh`.

## Start here
`installer-state.sh` for a change to install.sh, sot-apply.sh or lib/sot-daemon.sh: each `case_start` names the
behaviour it pins. For a Windows script change, the `.ps1` suite named for it above.

## Rules
- The five `.ps1` suites run only on the windows-latest leg of `rust.yml`, under Windows PowerShell 5.1; `rc-gate.sh`
  and a Linux box never run them. The step "Parse PowerShell scripts" globs `scripts/*.ps1` without recursion, so each
  `.ps1` suite parses itself and its siblings in its section 0.
- A suite runs only if a step of `.github/workflows/rust.yml` or a job of `rc-gate.sh` names it; a new suite is added
  to the step list in the commit that adds it. `rc-gate.sh` lists only the three shell suites here by name
  (`SHELL_ALL` in `producer`).
- `installer-support.sh` sources `install.sh` with `SOT_INSTALL_SOURCE_ONLY=1` and `lib/sot-daemon.sh` for both installer
  suites; `installer-state.sh` reads `rust/protocol/src/ops/lease.rs` for the pinned bounds (`launcher_bounds_match_ops`), so a
  rename there breaks it.
- The suites stub `sotd` (and `systemctl`, `nc` in `installer-support.sh`); none needs a network. `test-local-daemon.ps1`
  sections 3 to 6 need a real `sotd.exe`: a failure on CI when absent, a skip elsewhere.
- `rc-gate.sh` needs `CARGO_TARGET_DIR` to itself while it runs; its verdict ends `<logdir>/summary.txt` as `ALLDONE` or
  `ALLDONE FAILED`.
