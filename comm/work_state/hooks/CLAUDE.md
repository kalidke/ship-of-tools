# comm/work_state/hooks: the hooks that stamp a row's work-state (messaging)

Each hook is a thin script that a session's agent runs on an event and that stamps through `comm-status.sh` (the folder
above). They install flat into `~/.sot-comm/bin` beside it, and the agent settings name them there. Part of messaging;
charter: comm/CLAUDE.md.

## Files
- `comm-status-working.sh`: Claude `UserPromptSubmit`: sends `prompt`, with `COMM_STATUS_ORIGIN` naming who started the turn
- `comm-status-blocked.sh`: Claude `PreToolUse` on `AskUserQuestion`: stamps `blocked` with the question, then `stop`
- `comm-status-heartbeat.sh`: Claude `PostToolUse`: an `AskUserQuestion` answer sends a user `prompt`; any other call only refreshes `status_at`, at most once a minute, and writes no fact
- `comm-status-idle.sh`: Claude `Stop`: holds a turn with unread mail, stamps the closing marker, nudges, runs the auditor, then sends `stop`
- `codex-status-blocked.sh`: Codex `PermissionRequest` (`comm/adapters/codex/hooks.json`): stamps `blocked`, then `stop`

## Start here
`comm-status-idle.sh` for turn-end behaviour (its header numbers its jobs and the order they run in); the
others are short. The events the Claude hooks are wired to are `_COMM_STATE_HOOKS` in `src/claude_hooks.jl`; Codex wires
`working`, `heartbeat` and `idle` to its own events too.

## Rules
- A hook never blocks a session that is not a joined comm agent and never wedges a turn: each exits 0 on every path,
  and the Stop hook holds a turn only by printing a block decision.
- The four Claude hooks stand down on `SOT_COMM_HOOKS=off`. The Codex hook has no such check.
- Only the row's own agent stamps: every stamp goes through `comm-status.sh`, and `comm-status-idle.sh` and
  `comm-status-heartbeat.sh` also call `sot_require_agent` themselves.
- This folder holds hooks only. Installed, the Stop hook finds the auditor beside it in the bin; in the tree it must
  not, so a suite can run it with no live model (`comm/tests/test-status-floor.sh`).
