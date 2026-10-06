# T1 R2 hosted replay: c1-parent

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

- `c1-parent-quit_other_keys_cancel_and_are_consumed` on linux, macos, windows:
  Actual result: `test ui::input::keypress::tests::quit_other_keys_cancel_and_are_consumed ... FAILED`; exit `101`.
  Body: `T1 body entered: ui::input::keypress::tests::quit_other_keys_cancel_and_are_consumed`.
  Named failing assertion: `other key must cancel`.
  Fixture: `T1 fixture observed: routed raw key and downstream recorder`.
- `c1-parent-quit_enter_and_tab_use_identity_with_modifiers_and_rebindings` on linux, macos, windows:
  Actual result: `test ui::input::keypress::tests::quit_enter_and_tab_use_identity_with_modifiers_and_rebindings ... FAILED`; exit `101`.
  Body: `T1 body entered: ui::input::keypress::tests::quit_enter_and_tab_use_identity_with_modifiers_and_rebindings`.
  Named failing assertion: `logical Tab/Enter must win`.
  Fixture: `T1 fixture observed: routed raw key and downstream recorder`.
- `c1-parent-quit_modifier_presses_cancel_before_the_filter` on linux, macos, windows:
  Actual result: `test ui::input::keypress::tests::quit_modifier_presses_cancel_before_the_filter ... FAILED`; exit `101`.
  Body: `T1 body entered: ui::input::keypress::tests::quit_modifier_presses_cancel_before_the_filter`.
  Named failing assertion: `modifier must cancel before suppression`.
  Fixture: `T1 fixture observed: routed raw modifier and downstream recorder`.
- `c1-parent-prompt_pinned_cells_have_attention_style` on linux, macos, windows:
  Actual result: `test ui::chrome::view::tests::prompt_pinned_cells_have_attention_style ... FAILED`; exit `101`.
  Body: `T1 body entered: ui::chrome::view::tests::prompt_pinned_cells_have_attention_style`.
  Named failing assertion: `prompt foreground`.
  Fixture: `T1 fixture observed: rendered pinned prompt cells`.
- `c1-parent-quit_transitions_log_only_finite_fields` on linux, macos, windows:
  Actual result: `test ui::app::exit::tests::quit_transitions_log_only_finite_fields ... FAILED`; exit `101`.
  Body: `T1 body entered: ui::app::exit::tests::quit_transitions_log_only_finite_fields`.
  Named failing assertion: `missing transition event: quit prompt: open`.
  Fixture: `T1 fixture observed: captured actual quit transitions`.
- `c1-parent-leave_close_keep_and_handover_only_leave_leases` on linux, macos, windows:
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
