# T1 R2 hosted replay: c1-revert-paint

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

- `c1-revert-paint-prompt_pinned_cells_have_attention_style` on linux, macos, windows:
  Actual result: `test ui::chrome::view::tests::prompt_pinned_cells_have_attention_style ... FAILED`; exit `101`.
  Body: `T1 body entered: ui::chrome::view::tests::prompt_pinned_cells_have_attention_style`.
  Named failing assertion: `prompt foreground`.
  Fixture: `T1 fixture observed: rendered pinned prompt cells`.

## Evidence contract

One shared validator requires an actual exact named result, the specified failing assertion or post-assertion
green observation, exactly one substantive body entry and all fixture witnesses. Missing bodies, compile
failures, unrelated panics and unavailable fixtures fail. Synthetic controls demonstrate those rejections.
Unix C3a cleanup bodies never substitute for Windows compilation/private-pipe controls, or vice versa.
No native display-dependent C4 proof is included; C4 needs the merged D-A C12 startup seam.
