# rust/log/src/supervisor/journal: the supervisor's durable records and the operations that change them (capsule)

The supervisor keeps three records under a state dir: the operation journal (`supervisor-journal/`), the voyage pointer
(`drawer.voyage`) and the authority fence (`supervisor.lock`). This folder holds them and, after the next commit, the
end-run, reset and startup-recovery code that writes them. Part of the capsule; charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: the operation journal: `.active` and `.terminal` records, `begin`, `finish`, the readers
- `pointer.rs`: the voyage pointer `drawer.voyage`: path, publication, validation
- `fence.rs`: the authority fence `supervisor.lock` (`lock_supervisor`) and the daemon lock

## Start here
`begin` and `finish` in `mod.rs` for how an operation is recorded; `pointer_path` in `pointer.rs` for the pointer.

## Rules
- One immutable file per state per operation id: `.active` before the first irreversible act and `.terminal` once (`begin`, `finish`).
- File names are pinned here, never re-derived by callers (`pointer_path`, `supervisor_lock_path`).
