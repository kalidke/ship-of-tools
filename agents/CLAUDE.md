# agents/: the edge with agent programs (charter)

## Idea
Ship of Tools meets an agent program (Claude Code, Codex) at one edge. The daemon starts the program from one launch
recipe (argv, environment, account, folder trust), and the program drives Ship of Tools only through the daemon's wire,
using the tools and skills installed from the repository. This folder is the shell side of that edge; the daemon side is
`rust/backend/src/agents/`. At this commit the row-lifecycle CLIs, the /worktree scripts, the frontend CLI, the launchers, the skills
and the GitHub sign-in live here; the rest of the shell client is still under `comm/` and moves in later units of the organize pass.

## Owns
- The offline trust declaration command and its typed schema.
- The CLIs that start, end, probe and bootstrap rows: `spawn/`.
- The /worktree scripts, one parallel session per git worktree: `worktree/`.
- The CLI that drives the frontend and its REPL, and the nav broadcast: `sot-fe/`.
- The Claude launcher `ccb`, its PATH wrappers (`show-result`, `sot-fe`, `sot-gh-auth`) and the eleven skills that are
  not messaging's (`julia-repl`, `project-log`, `reauth`, `show-result`, `sitrep`, `sot-gh-auth`, `sot-install`,
  `sot-setup`, `sot-status`, `sot-statusline-setup`, `worktree`): `claude/`. The Codex launcher `ccx`: `codex/`.
- `sot-gh-auth.sh`, the GitHub device-flow sign-in.
- Their suites: `tests/`.
- Still elsewhere, listed so a reader finds them:
  - the shell daemon client in `comm/lib/comm-lib-client.sh` (`sot_daemon_endpoint`, `sot_relay_endpoint`,
    `sot_oneshot_request`, `sot_pty_input`);
  - the hooks, the Codex skills and plugin, and the two messaging skills (`sot-comm`, `sot-session-start`) in
    `comm/adapters/claude/` and `comm/adapters/codex/`: messaging's.
- The daemon side: `agent_argv` and `agent_exec_argv` (argv per agent kind), `account_env`, `ensure_folder_trusted`,
  the `SOT_*` awareness env, `sotd agent-exec` and the `accounts.list` op, all in `rust/backend/src/agents/`.

## Promises
- Every claude launcher passes `--permission-mode auto`, never `--dangerously-skip-permissions`: `claude_recipe` in the
  daemon's `argv.rs`; `ccb` execs `sotd agent-exec claude`, which carries the same recipe.
- Every leg starts a fresh conversation: the capsule recipes carry no resume token (`claude_argv`, `codex_argv`).
  A conversation is resumed only when asked: by a reauth's own leg (`claude_resume_argv`'s `--resume <id>`, passed to
  the supervisor as `--first-leg-only`, so no later leg or watchdog restart carries it), or by a person (`ccb
  --continue`, `ccx --continue`; a bare `ccb` or `ccx` starts fresh). A fresh session reads its handoff from the `HANDOFF:` line
  that `sot_handoff_line` (comm library) gives `comm-session-start.sh` and `ccx`.
- ccx's default handle `<repo>-cx-<host>` is built from the comm library's safe pieces (`sot_sanitize_component`, `_sot_handle_host`), so it is always a name `workspace.create` accepts; a derivation that cannot run stops the launch before the join and before codex (`agents/tests/test-ccx-launch.sh`).
- Folder trust is recorded only when the OS-resolved root lies under the OS-resolved declared prefix. The key uses the child cwd's spelling; an already accepted entry is not rewritten. Outside, undeclared and failed preparation are observable and the agent still starts. The trust file follows the child's effective `CLAUDE_CONFIG_DIR`: account additions override inheritance; with no config override it is the home-level file. Automated evidence establishes preparation; actual child-config consumption remains a human done-test proof limit. Interactive trust recognition, parent coverage and no-dialog behavior remain outside the headless proof.
- The daemon's own `PATH` need not hold `~/.local/bin` (as a Linux user service it carries only what `~/.bashrc`
  exports above its non-interactive guard: `deploy/sotd.service`). So on Unix the daemon launches `claude` and `ccx`
  by absolute path, searching `PATH` and then `~/.local/bin`, and puts `~/.local/bin` at the front of a leg's `PATH`
  that lacks it (`agent_env`). `ccb` resolves `sotd` from `SOTD_BIN`, `PATH`, then the install paths.
- Every endpoint leaves the shell client through `_sot_emit_endpoint` (a whitelist of `unix:`, `pipe:` and `ssh:`);
  an explicit endpoint that it refuses is fatal; `sot_relay_endpoint` is what `sotd topology relay-endpoint` answers,
  takes no argument or variable override, and never falls back to the local daemon.
- Every code file directly in this folder installs flat into `~/.sot-comm/bin`, because `comm/bin-folders.txt` lists
  `agents`; `claude/` and `codex/` are never in that list, so their files install only through `src/sources.jl`.
- A skill body is a template: Claude Code substitutes `$<digit>` and `$ARGUMENTS` into the text before the model reads
  it, so a skill carries no `$` followed by a digit; the sot-setup skill's launcher script finds its folder by
  `BASH_SOURCE`, not `$0`. No test enforces the rule.

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: `sotd trust declare <absolute-prefix>`, `agent_argv`, `agent_exec_argv`,
`claude_recipe`, `account_env`, `account_spawn_env`, `ensure_folder_trusted`, `capsule_supervisor_env`,
`comm/lib/comm-lib-client.sh`, `sot_daemon_endpoint`, `sot_relay_endpoint`, `sot_oneshot_request`, `sot_pty_input`,
`sot_dial`, `SOT_COMM_NAME`, `SOT_COMM_HOME`, `SOT_COMM_SELF_FILE`, `sot-fe preview`, `docs/tools/docs-media.sh`. Uses:
`sot_hello_frame`, `comm/lib/comm-lib-client.sh`, `sot_ssh_bridge`, `_sot_is_plain_host_name`, `sot_slug`,
`comm/lib/comm-lib-identity.sh`, `SshRecipe`, `is_plain_host_name`, `slug`, `sotd stdio-bridge`, `sotd topology relay-endpoint`,
`relay_endpoint_cmd`, `_sot_planned_relay_endpoint`, `version.query`,
`workspace.create`,
`workspace.destroy`, `workspace.list`, `workspace.reauth`, `pty.input`, `pty.screen`, `workspace.changed`, `sot_host`,
`comm/lib/comm-lib-base.sh`, `host_name`, `comm-context.sh`, `comm-join.sh`, `comm-relay.sh`, `comm-poll.sh`,
`agents/spawn/comm-probe.sh`, `agents/spawn/comm-bootstrap.sh`, `fe.command.send`, `fe.command`,
`agents/sot-fe/sot-fe-request.sh`, `sot_ui`, `agent.message`, `agents/sot-fe/sot-nav.sh`, `comm-relay.sh send --all`,
`install_comm`, `update_comm`, `comm/bin-folders.txt`, `src/sources.jl`, `~/.sot-comm/bin`, `sot_sanitize_component`, `_sot_handle_host`. Uses also: HOST, HANDLE_HOST and _sot_self_slot from the messaging context/library.

## Folders
- `spawn/`: the four row-lifecycle CLIs (see its page).
- `worktree/`: the four /worktree scripts (see its page).
- `sot-fe/`: the frontend-and-REPL CLI and the nav broadcast (see its page).
- `claude/`: the Claude launcher, its wrappers and the non-messaging skills (installer-shaped, no page).
- `codex/`: the Codex launcher (installer-shaped, no page).
- `tests/`: the hermetic suites of those CLIs (see its page).
- `rust/backend/src/agents/`: the daemon's launch recipe, accounts and folder trust.
- `comm/lib/`, `comm/adapters/`: where the rest of the agent edge still lives (see Owns).

## Files
- `claude/`: the Claude launcher `bin/ccb`, the wrappers `bin/show-result`, `bin/sot-fe`, `bin/sot-gh-auth`, and the skills.
- `codex/`: the Codex launcher `bin/ccx`.
- `sot-gh-auth.sh`: GitHub CLI sign-in by the device flow, without a browser on the box.
- `spawn/`: the CLIs that start, end, probe and bootstrap rows.
- `worktree/`: the scripts that make, list, remind and remove a session's git worktree.
- `sot-fe/`: the CLI that drives the frontend and its REPL, and `sot-nav.sh`.
- `tests/`: the suites that prove them.

## Start here
For a change to how a row and its agent start, `spawn/comm-spawn.sh`, then `agent_argv` in
`rust/backend/src/agents/argv.rs`. For how a shell tool reaches the daemon, `sot_daemon_endpoint` in
`comm/lib/comm-lib-client.sh`.
