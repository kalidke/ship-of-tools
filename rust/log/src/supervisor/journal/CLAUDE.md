# rust/log/src/supervisor/journal: the supervisor's durable records and the operations that change them (capsule)

The supervisor keeps three records under a state dir: the operation journal (`supervisor-journal/`), the voyage pointer
(`drawer.voyage`) and the authority fence (`supervisor.lock`). This folder holds them and the end-run, reset and
startup-recovery code that writes them. Part of the capsule; charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: the operation journal: `.active` and `.terminal` records, `begin`, `finish`, the readers
- `tests.rs`: the journal's unit tests
- `end_run.rs`: ending a voyage over its management lane, and reconciling it through the leg's run-end marker
- `reset.rs`: `reset_pointer` and `reconcile_reset`, the pointer rename-aside and the recovery of a crashed reset
- `recover.rs`: `reconcile_journal_on_startup`, the sweep of active entries before the pointer is read
- `pointer.rs`: the voyage pointer `drawer.voyage`: path, publication, validation
- `fence.rs`: the authority fence `supervisor.lock` (`lock_supervisor`)

## Start here
`begin` and `finish` in `mod.rs` for how an operation is recorded; `reconcile_journal_on_startup` in `recover.rs` for restart; `end_run_over_mgmt_lane` in `end_run.rs` for ending a run.

## Rules
- One immutable file per state per operation id: `.active` before the first irreversible act and `.terminal` once (`begin`, `finish`).
- File names are pinned here, never re-derived by callers (`pointer_path`, `supervisor_lock_path`).
- A run-end marker alone never proves the writer gone: `probe_writer_liveness` runs before `reconcile_via_marker` (`finish_end_run_without_process`).
- Reset renames the old pointer aside without replacing and deletes nothing (`reset_pointer`).
- Every active entry is finished before the pointer is read (`reconcile_journal_on_startup`).
- A publication creates its temp file exclusively and removes it on any write, sync or rename failure (`publish_json`).
- `SOT_TEST_JOURNAL_PUBLISH_DELAY_MS`, read once per process and set only by tests, holds every journal publication that long (`publish_json`).
- `reset_pointer` returns a storage-exhaustion error as itself through flush, rename, bootstrap and pointer publication; other errors keep their `State` text.
