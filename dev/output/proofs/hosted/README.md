# T1 R2 hosted replay: c3-head

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

- `c3-head-blocked_pool_ok_cleans_yielding_child` on linux, macos, windows:
  Actual result: `test ui::app::exit_process_tests::blocked_pool_ok_cleans_yielding_child ... ok`; exit `0`.
  Body: `T1 body entered: ui::app::exit_process_tests::blocked_pool_ok_cleans_yielding_child`.
  After the actual assertion: `T1 assertion passed: runtime finalization exceeded two seconds:`.
  Fixture: `T1 fixture observed: original child entered once and remained alive`.
  Fixture: `T1 fixture observed: yielding task and blocked pool ready`.
  Fixture: `T1 fixture observed: yielding ownership dropped and original child exited`.
- `c3-head-blocked_pool_error_cleans_yielding_child` on linux, macos, windows:
  Actual result: `test ui::app::exit_process_tests::blocked_pool_error_cleans_yielding_child ... ok`; exit `0`.
  Body: `T1 body entered: ui::app::exit_process_tests::blocked_pool_error_cleans_yielding_child`.
  After the actual assertion: `T1 assertion passed: error return bypassed runtime finalization`.
  Fixture: `T1 fixture observed: original child entered once and remained alive`.
  Fixture: `T1 fixture observed: yielding task and blocked pool ready`.
  Fixture: `T1 fixture observed: yielding ownership dropped and original child exited`.
- `c3-head-held_worker_exposes_delayed_child_destruction` on linux, macos, windows:
  Actual result: `test ui::app::exit_process_tests::held_worker_exposes_delayed_child_destruction ... ok`; exit `0`.
  Body: `T1 body entered: ui::app::exit_process_tests::held_worker_exposes_delayed_child_destruction`.
  After the actual assertion: `T1 assertion passed: runtime finalization exceeded two seconds:`.
  Fixture: `T1 fixture observed: original child entered once and remained alive`.
  Fixture: `T1 fixture observed: yielding task and held worker ready`.
  Fixture: `T1 fixture observed: yielding ownership dropped and original child exited`.
- `c3-head-absent_runtime_and_repeated_finalization_preserve_results` on linux, macos, windows:
  Actual result: `test ui::app::exit_process_tests::absent_runtime_and_repeated_finalization_preserve_results ... ok`; exit `0`.
  Body: `T1 body entered: ui::app::exit_process_tests::absent_runtime_and_repeated_finalization_preserve_results`.
  After the actual assertion: `T1 assertion passed: None runtime and repeated finalization preserve Ok and Err`.
  Fixture: `T1 fixture observed: absent runtime and repeated finalization`.

## Evidence contract

One shared validator requires an actual exact named result, the specified failing assertion or post-assertion
green observation, exactly one substantive body entry and all fixture witnesses. Missing bodies, compile
failures, unrelated panics and unavailable fixtures fail. Synthetic controls demonstrate those rejections.
Unix C3a cleanup bodies never substitute for Windows compilation/private-pipe controls, or vice versa.
No native display-dependent C4 proof is included; C4 needs the merged D-A C12 startup seam.
