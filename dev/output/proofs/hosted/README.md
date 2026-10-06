# T1 R2 hosted replay: c2-revert-timer

## Execution

Local replay branch only; do not merge its scaffold, reversals or workflow changes into the lane.
Dispatch `.github/workflows/rust.yml`, job `test` (`build+test`), on ubuntu-latest, macos-latest and windows-latest.
The step `Validate T1 replay observations` invokes `python3 dev/output/proofs/hosted/run.py`; no display is required.
The validator runs before ordinary whole-workspace tests. Intended named reds are successful replay observations,
while the later ordinary suite retains its failures. Unrelated failures remain separately classified.
Each exact run has a nine-minute Python deadline, uses nice, eight cargo jobs, locked offline dependencies and
line-tables-only debug information. The preceding workspace build and all-target Clippy gates supply test dependencies.
The runner archives complete raw logs, exit codes, recorded child PIDs, validator outcomes and controls in
`dev/output/proofs/hosted/logs/`; the next always-run step uploads them per platform.
`--local` excludes previously denied socket fixtures and cannot claim a hosted/platform verdict.

## Required observations

- `c2-revert-timer-frozen_worker_and_multiple_holders_share_one_deadline` on linux, macos, windows:
  Actual result: `test lease::delivery_tests::frozen_worker_and_multiple_holders_share_one_deadline ... FAILED`; exit `101`.
  Body: `T1 body entered: lease::delivery_tests::frozen_worker_and_multiple_holders_share_one_deadline`.
  Named failing assertion: `transport worker extended forced delivery:`.
  Fixture: `T1 fixture observed: frozen worker and responsive mixed holders`.
- `c2-revert-timer-forced_exit_delivers_the_queued_close` on linux, macos, windows:
  Actual result: `test lease::leave_tests::forced_exit_delivers_the_queued_close ... ok`; exit `0`.
  Body: `T1 body entered: lease::leave_tests::forced_exit_delivers_the_queued_close`.
  After the actual assertion: `T1 assertion passed: the queued close reaches the daemon before eof:`.
  Fixture: `T1 fixture observed: accepted Keep and queued Close before EOF`.

## Evidence contract

One shared validator requires an actual exact named result, the specified failing assertion or post-assertion
green observation, exactly one substantive body entry and all fixture witnesses. Missing bodies, compile
failures, unrelated panics and unavailable fixtures fail. Synthetic controls demonstrate those rejections.
Unix C3a cleanup bodies never substitute for Windows compilation/private-pipe controls, or vice versa.
No native display-dependent C4 proof is included; C4 needs the merged D-A C12 startup seam.
