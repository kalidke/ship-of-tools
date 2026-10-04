# agents/: the edge with agent programs (charter)

## Idea
Ship of Tools meets an agent program (Claude Code, Codex) at one edge. The daemon starts the program from one launch
recipe (argv, environment, account, folder trust), and the program drives Ship of Tools only through the daemon's wire,
using the tools and skills installed from the repository. This folder is the shell side of that edge; the daemon side is
`rust/backend/src/agents/`. At this commit only the row-lifecycle CLIs live here; the rest is still under `comm/` and
moves in later units of the organize pass.

## Owns
- The CLIs that start, end, probe and bootstrap rows: `spawn/`.
- Their suites: `tests/`.
- Still elsewhere, listed so a reader finds them:
  - the shell daemon client in `comm/core/scripts/comm-lib.sh` (`sot_daemon_endpoint`, `sot_relay_endpoint`,
    `sot_oneshot_request`, `sot_pty_input`) and `comm/core/scripts/comm-pipe-request.ps1`;
  - the other daemon-client CLIs in `comm/core/scripts/`: `sot-fe`, `comm-worktree-*.sh`, `sot-nav.sh`, `sot-gh-auth.sh`;
  - the launchers `ccb` and `ccx`, the PATH wrappers and the skills in `comm/adapters/claude/` and
    `comm/adapters/codex/` (the two comm skills there are messaging's).
- The daemon side: `agent_argv` and `agent_exec_argv` (argv per agent kind), `account_env`, `ensure_folder_trusted`,
  the `SOT_*` awareness env, `sotd agent-exec` and the `accounts.list` op, all in `rust/backend/src/agents/`.

## Promises
- Every claude launcher passes `--permission-mode auto`, never `--dangerously-skip-permissions`: `claude_recipe` in the
  daemon's `argv.rs`; `ccb` execs `sotd agent-exec claude`, which carries the same recipe; the sot-setup skill's
  `resume_command` names the flag.
- Folder trust is written only for a row root under `[trust] root_prefix` in the user-level settings file
  (`ensure_folder_trusted`); an entry already accepted is never rewritten.
- A launcher the daemon spawns full-paths its binaries: its environment lacks `~/.local/bin`. `ccb` resolves `sotd`
  from `SOTD_BIN`, `PATH`, then the install paths; the daemon resolves `claude` and `ccx` to absolute paths on Unix.
- Every endpoint leaves the shell client through `_sot_emit_endpoint` (a whitelist of `unix:`, `pipe:` and `ssh:`);
  an explicit endpoint that it refuses is fatal; `sot_relay_endpoint` never falls back to the local daemon.
- A skill body is a template: Claude Code substitutes `$<digit>` and `$ARGUMENTS` into the text before the model reads
  it, so a skill carries no `$` followed by a digit. Not yet true of the sot-setup skill, whose installer script line
  `REPO="$(cd "$(dirname "$0")/.." ...` carries a `$0` (both copies); the unit that moves the skills must fix it.

## Connections
- Out, by op, over the daemon's wire through the shell client: `workspace.create` (`spawn/comm-spawn.sh`, and
  `spawn/comm-probe.sh` for its probe rows), `workspace.list` (all four), `workspace.destroy`
  (`spawn/comm-despawn.sh`), `pty.input` (`spawn/comm-bootstrap.sh`, `spawn/comm-probe.sh`), `version.query` (the
  declared host a spawn lands on).
- In: `comm-lib.sh` (sourced from the script's own folder, flat once installed), `comm-context.sh` (identity), and
  `comm-join.sh`, `comm-relay.sh` and `comm-poll.sh` (run by `spawn/comm-probe.sh`'s responder).
- The installer copies the files of `spawn/` flat into `~/.sot-comm/bin` (`comm/bin-folders.txt`); the suites run from
  a staged copy of the same list (`comm/tests/stage-bin.sh`).
- Messaging's scripts call none of this folder's scripts; `comm-worktree-new.sh` and `comm-worktree-clean.sh` call
  `comm-spawn.sh` and `comm-despawn.sh` by their installed names.

## Folders
- `spawn/`: the four row-lifecycle CLIs (see its page).
- `tests/`: the hermetic suites of those CLIs (see its page).
- `rust/backend/src/agents/`: the daemon's launch recipe, accounts and folder trust.
- `comm/core/scripts/`, `comm/adapters/`: where the rest of the agent edge still lives (see Owns).

## Files
- `spawn/`: the CLIs that start, end, probe and bootstrap rows.
- `tests/`: the suites that prove them.

## Start here
For a change to how a row and its agent start, `spawn/comm-spawn.sh`, then `agent_argv` in
`rust/backend/src/agents/argv.rs`. For how a shell tool reaches the daemon, `sot_daemon_endpoint` in
`comm/core/scripts/comm-lib.sh`.
