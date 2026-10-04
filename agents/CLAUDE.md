# agents/: the edge with agent programs (charter)

## Idea
Ship of Tools meets an agent program (Claude Code, Codex) at one edge. The daemon starts the program from one launch
recipe (argv, environment, account, folder trust), and the program drives Ship of Tools only through the daemon's wire,
using the tools and skills installed from the repository. This folder is the shell side of that edge; the daemon side is
`rust/backend/src/agents/`. At this commit the row-lifecycle CLIs, the /worktree scripts, the frontend CLI, the launchers, the skills
and the GitHub sign-in live here; the rest of the shell client is still under `comm/` and moves in later units of the organize pass.

## Owns
- The CLIs that start, end, probe and bootstrap rows: `spawn/`.
- The /worktree scripts, one parallel session per git worktree: `worktree/`.
- The CLI that drives the frontend and its REPL, and the nav broadcast: `sot-fe/`.
- The Claude launcher `ccb`, its PATH wrappers (`show-result`, `sot-fe`, `sot-gh-auth`) and the eleven skills that are
  not messaging's (`julia-repl`, `project-log`, `reauth`, `show-result`, `sitrep`, `sot-gh-auth`, `sot-install`,
  `sot-setup`, `sot-status`, `sot-statusline-setup`, `worktree`): `claude/`. The Codex launcher `ccx`: `codex/`.
- `sot-gh-auth.sh`, the GitHub device-flow sign-in; and `comm-pipe-request.ps1`, the shell client's pipe transport
  (the Windows request a `pipe:` endpoint makes, called by `comm-lib.sh` and `comm-relay.sh`).
- Their suites: `tests/`.
- Still elsewhere, listed so a reader finds them:
  - the shell daemon client in `comm/core/scripts/comm-lib.sh` (`sot_daemon_endpoint`, `sot_relay_endpoint`,
    `sot_oneshot_request`, `sot_pty_input`);
  - the hooks, the Codex skills and plugin, and the two messaging skills (`sot-comm`, `sot-session-start`) in
    `comm/adapters/claude/` and `comm/adapters/codex/`: messaging's.
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
- Every code file directly in this folder installs flat into `~/.sot-comm/bin`, because `comm/bin-folders.txt` lists
  `agents`; `claude/` and `codex/` are never in that list, so their files install only through `src/sources.jl`.
- A skill body is a template: Claude Code substitutes `$<digit>` and `$ARGUMENTS` into the text before the model reads
  it, so a skill carries no `$` followed by a digit; the sot-setup skill's launcher script finds its folder by
  `BASH_SOURCE`, not `$0`. No test enforces the rule.

## Connections
- Out, by op, over the daemon's wire through the shell client: `workspace.create` (`spawn/comm-spawn.sh`, and
  `spawn/comm-probe.sh` for its probe rows), `workspace.list` (all four), `workspace.destroy`
  (`spawn/comm-despawn.sh`), `pty.input` (`spawn/comm-bootstrap.sh`, `spawn/comm-probe.sh`), `version.query` (the
  declared host a spawn lands on).
- In: `comm-lib.sh` (sourced from the script's own folder, flat once installed), `comm-context.sh` (identity), and
  `comm-join.sh`, `comm-relay.sh` and `comm-poll.sh` (run by `spawn/comm-probe.sh`'s responder).
- The installer copies the files of `spawn/`, `worktree/` and `sot-fe/` and the code files directly in this folder flat into
  `~/.sot-comm/bin` (`comm/bin-folders.txt`); the suites run from a staged copy of the same list (`comm/tests/stage-bin.sh`).
- `claude/` and `codex/` are installed by `src/sources.jl`: `claude/` is a skill root and `claude/bin` and `codex/bin`
  are the launcher folders, copied into `~/.local/bin`; no CLAUDE.md in them is installed.
- Messaging's scripts call none of this folder's scripts; `comm-worktree-new.sh` and `comm-worktree-clean.sh` call
  `comm-spawn.sh` and `comm-despawn.sh` by their installed names.

## Folders
- `spawn/`: the four row-lifecycle CLIs (see its page).
- `worktree/`: the four /worktree scripts (see its page).
- `sot-fe/`: the frontend-and-REPL CLI and the nav broadcast (see its page).
- `claude/`: the Claude launcher, its wrappers and the non-messaging skills (installer-shaped, no page).
- `codex/`: the Codex launcher (installer-shaped, no page).
- `tests/`: the hermetic suites of those CLIs (see its page).
- `rust/backend/src/agents/`: the daemon's launch recipe, accounts and folder trust.
- `comm/core/scripts/`, `comm/adapters/`: where the rest of the agent edge still lives (see Owns).

## Files
- `claude/`: the Claude launcher `bin/ccb`, the wrappers `bin/show-result`, `bin/sot-fe`, `bin/sot-gh-auth`, and the skills.
- `codex/`: the Codex launcher `bin/ccx`.
- `comm-pipe-request.ps1`: the shell client's pipe transport, a PowerShell client for a `pipe:` endpoint.
- `sot-gh-auth.sh`: GitHub CLI sign-in by the device flow, without a browser on the box.
- `spawn/`: the CLIs that start, end, probe and bootstrap rows.
- `worktree/`: the scripts that make, list, remind and remove a session's git worktree.
- `sot-fe/`: the CLI that drives the frontend and its REPL, and `sot-nav.sh`.
- `tests/`: the suites that prove them.

## Start here
For a change to how a row and its agent start, `spawn/comm-spawn.sh`, then `agent_argv` in
`rust/backend/src/agents/argv.rs`. For how a shell tool reaches the daemon, `sot_daemon_endpoint` in
`comm/core/scripts/comm-lib.sh`.
