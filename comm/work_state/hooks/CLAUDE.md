# comm/work_state/hooks: the hooks that stamp a row's work-state (messaging)

Each hook is a thin script that a session's agent runs on an event and that stamps through `comm-status.sh` (the folder
above). They install flat into `~/.sot-comm/bin` beside it, and the agent settings name them there. Part of messaging;
charter: comm/CLAUDE.md.

## Files
- `comm-status-working.sh`: Claude `UserPromptSubmit`: sends `prompt`, with `COMM_STATUS_ORIGIN` naming who started the turn
- `comm-status-blocked.sh`: Claude `PreToolUse` on `AskUserQuestion`: stamps `blocked` with the question, then `stop`
- `comm-subagent-guard.sh`: Claude `PreToolUse` on `Bash`: when the hook input has an `agent_id` (a subagent) and the command names a `comm-*.sh` script, prints the deny decision; otherwise allows silently
- `comm-status-heartbeat.sh`: Claude and Codex `PostToolUse`: with a dialog marker for the row, sends `resumed`; an `AskUserQuestion` answer sends a user `prompt`; any other call only refreshes `status_at`, at most once a minute, and writes no fact
- `comm-status-idle.sh`: Claude `Stop`: holds a turn with unread mail, stamps the closing marker, nudges, runs the auditor, then sends `stop`
- `codex-status-blocked.sh`: Codex `PermissionRequest` (`comm/adapters/codex/hooks.json`): stamps `dialog`

## Start here
`comm-status-idle.sh` for turn-end behaviour (its header numbers its jobs and the order they run in); the
others are short. The events the Claude hooks are wired to are `_COMM_STATE_HOOKS` in `src/claude_hooks.jl`; Codex wires
`working`, `heartbeat` and `idle` to its own events too.

## Rules
- A hook never blocks a session that is not a joined comm agent and never wedges a turn: each exits 0 on every path,
  and the Stop hook holds a turn only by printing a block decision.
- `comm-subagent-guard.sh` refuses only with a non-empty `agent_id` in its input; with none, jq missing or input that does not parse it allows, so a session is never locked out of its own mail.
- The four work-state Claude hooks and the Codex permission hook stand down on SOT_COMM_HOOKS=off before reading input, resolving paths or stamping a row.
- Only the row's own agent stamps: every stamp goes through `comm-status.sh`, and `comm-status-idle.sh` and
  `comm-status-heartbeat.sh` also call `sot_require_agent` themselves.
- This folder holds hooks only. Installed, the Stop hook finds the auditor beside it in the bin; in the tree it must
  not, so a suite can run it with no live model (`comm/tests/test-status-floor.sh`).
- Heartbeat context resolution uses the library's `sot_bounded` owner. A context timeout, setup failure or cancellation discards its entire output and skips the stamp; the hook adds no separate child wait. Its stdout and stderr files (`state/.hb-ctx-<pid>` and `.err`, made exclusively) are removed after the bounded invocation, and by the call's subshell's EXIT trap however the call ends, so a TERM, HUP or INT that ends the hook leaves neither, a second one included (the subshell arms itself and runs the hook's copy of `sot_cleanup_signals`, and its trap ignores the three: comm/lib/comm-lib-base.sh, cleanup traps); SIGKILL leaves both, and so does a signal in the instant between a file's creation and its name reaching the trap; the trap never removes a file the call did not make. Diagnostics are replayed from a finite stderr snapshot, and an ordinary completed context call retains its stdout even when it exits nonzero.
- The Stop hook's main shell holds its feedback record under a cleanup trap and `sot_cleanup_signals`
  (comm/lib/comm-lib-base.sh, cleanup traps), so a signal sent to the hook's pid alone acts when its foreground
  command ends, the turn auditor's `claude -p` included (45 s at most); a signal to the group ends that command too.
