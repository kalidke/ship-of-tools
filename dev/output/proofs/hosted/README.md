# T1 R2 hosted replay: c1-revert-gate

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

- `c1-revert-gate-quit_modifier_presses_cancel_before_the_filter` on linux, macos, windows:
  Actual result: `test ui::input::keypress::tests::quit_modifier_presses_cancel_before_the_filter ... FAILED`; exit `101`.
  Body: `T1 body entered: ui::input::keypress::tests::quit_modifier_presses_cancel_before_the_filter`.
  Named failing assertion: `modifier must cancel before suppression`.
  Fixture: `T1 fixture observed: routed raw modifier and downstream recorder`.
- `c1-revert-gate-leave_close_keep_and_handover_only_leave_leases` on linux, macos, windows:
  Actual result: `test ui::app::exit::tests::leave_close_keep_and_handover_only_leave_leases ... ok`; exit `0`.
  Body: `T1 body entered: ui::app::exit::tests::leave_close_keep_and_handover_only_leave_leases`.
  After the actual assertion: `T1 assertion passed: exit state is set before the UI effect`.
  Fixture: `T1 fixture observed: actual lease frames and exit effects`.

## Evidence contract

One shared validator requires an actual exact named result, the specified failing assertion or post-assertion
green observation, exactly one substantive body entry and all fixture witnesses. Missing bodies, compile
failures, unrelated panics and unavailable fixtures fail. Synthetic controls demonstrate those rejections.
Unix C3a cleanup bodies never substitute for Windows compilation/private-pipe controls, or vice versa.
No native display-dependent C4 proof is included; C4 needs the merged D-A C12 startup seam.
