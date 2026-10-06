# T1 R2 hosted replay: c3a-revert

## Execution

Local replay branch only; do not merge its scaffold, reversals or workflow changes into the lane.
Dispatch `.github/workflows/rust.yml`, job `test` (`build+test`), on ubuntu-latest, macos-latest and windows-latest.
The step `Validate T1 replay observations` invokes `python3 dev/output/proofs/hosted/run.py`; no display is required.
The validator runs before ordinary whole-workspace tests. Intended named reds are successful replay observations,
while the later ordinary suite retains its failures. Unrelated failures remain separately classified.
Each exact run has a nine-minute Python deadline, uses nice, eight cargo jobs, locked offline dependencies and
line-tables-only debug information. Hosted Build workspace first supplies the dependency cache.
The runner archives complete raw logs, exit codes, recorded child PIDs, validator outcomes and controls in
`dev/output/proofs/hosted/logs/`; the next always-run step uploads them per platform.
`--local` excludes previously denied socket fixtures and cannot claim a hosted/platform verdict.

## Required observations

- `c3a-revert-bind_failure_removes_its_folder` on linux, macos:
  Actual result: `test lease::grant_tests::bind_failure_removes_its_folder ... FAILED`; exit `101`.
  Body: `T1 body entered: lease::grant_tests::bind_failure_removes_its_folder`.
  Named failing assertion: `private lease listener directory is removed after a bind failure`.
  Fixture: `forced occupied-path bind failure observed`.
- `c3a-revert-bind_removes_its_folder_when_dropped` on linux, macos:
  Actual result: `test lease::grant_tests::bind_removes_its_folder_when_dropped ... ok`; exit `0`.
  Body: `T1 body entered: lease::grant_tests::bind_removes_its_folder_when_dropped`.
  After the actual assertion: `T1 assertion passed: was left behind`.
  Fixture: `T1 fixture observed: successful listener owns its private directory`.
- `c3a-revert-harness_never_leases` on windows:
  Actual result: `test lease::grant_tests::harness_never_leases ... ok`; exit `0`.
  Body: `T1 body entered: lease::grant_tests::harness_never_leases`.
  After the actual assertion: `T1 assertion passed: an exempt window must not connect`.
  Fixture: `T1 fixture observed: private listener bound and exempt lease refused connection`.

## Evidence contract

One shared validator requires an actual exact named result, the specified failing assertion or post-assertion
green observation, exactly one substantive body entry and all fixture witnesses. Missing bodies, compile
failures, unrelated panics and unavailable fixtures fail. Synthetic controls demonstrate those rejections.
Unix C3a cleanup bodies never substitute for Windows compilation/private-pipe controls, or vice versa.
No native display-dependent C4 proof is included; C4 needs the merged D-A C12 startup seam.
