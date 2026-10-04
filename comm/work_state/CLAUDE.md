# comm/work_state: stamp and reduce a row's work-state (messaging)

A row's colour is a reduction of facts. A session stamps one fact at a time (`floor`, `question`, `waiting`, `done`, and
the `note` line), and `comm-status.sh` reduces them to the row's `state` and `summary` in the registry. Part of
messaging; charter: comm/CLAUDE.md. The design of record is docs/adr/0044 and its amendment.

## Files
- `comm-status.sh`: writes one fact (`prompt`, `stop`, `working`, `idle`, `blocked`, `done`, `waiting`) and reduces the facts to state and summary in one jq program
- `comm-turn-auditor.sh`: the Stop hook's auditor: cheap pre-filters on the turn's last reply, then one headless Haiku `claude -p` call that returns findings
- `hooks/`: the Claude and Codex hooks that send the events and the closing-marker stamps (see its page)

## Start here
`comm-status.sh`: its header lists the verbs, and the reduction table (blocked, working, waiting, done, idle, by
priority) sits above the `with_lock` call that applies it. For what the Stop hook does with a finding, read
`hooks/comm-status-idle.sh` and the auditor's header.

## Rules
- The verb, the reduction and the stamp are one jq program run inside `with_lock`, so a writer that commits between
  another's read and write is never overwritten.
- A declaration (`working`, `idle`, `blocked`, `done`, `waiting`) with nowhere to land fails loudly; an event
  (`prompt`, `stop`) from a session with no registry row is a silent no-op (`sot_require_agent`).
- The daemon stamps no fact; it clears only `done`, when a person views the row (`clear_comm_unread` in
  `rust/backend/src/comm/registry/registry.rs`).
- The auditor fails open: any internal failure, a missing `claude`, `SOT_TURN_AUDITOR=0` or `auditor.off` in the comm
  folder exits 3, and the Stop hook then takes its old path. It never holds a turn.
- The auditor runs its model call with `SOT_COMM_HOOKS=off`, so the headless call's own hooks stand down.
