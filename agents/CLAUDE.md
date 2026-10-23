# agents: the edge with agent programs (contract)

Ship of Tools meets an agent program (Claude Code, Codex) at one edge. The daemon starts the program from one launch
recipe (argv, environment, account, folder trust), and the program drives Ship of Tools only through the daemon's wire,
using the tools and skills installed from the repository. Two halves: the daemon's, in `rust/backend/src/agents/` (the
launch recipe: `argv.rs`, `env.rs`, `awareness.rs`, `accounts.rs`, `account_links.rs`, `folder_trust.rs`,
`trust_declaration.rs`, `memory.rs`, `layer.rs` and the ops in `ops.rs`), and the shell's, in this folder (the launchers,
the row CLIs, the skills). Pass 10a wrote this contract for the whole subsystem and proved the daemon's half and the
skills' placement; pass 10b settles the launchers and the row CLIs and marks its sections below.

## Guarantees
Each states what the rest may rely on, the mechanism it rests on and the limit it has. The Tests section holds the test
of each.

1. **One recipe builds every Claude leg, and every leg runs `--permission-mode auto`.** `claude_recipe` (`argv.rs`) is
   the one builder of the flag shape: `claude --permission-mode auto [--settings <json>] <caller's flags>
   /sot-session-start`, with the layer's `--add-dir` in front when the layer is installed (guarantee 7). The capsule
   leg (`claude_argv`), the reauth leg (`claude_resume_argv`) and `sotd agent-exec claude` (`agent_exec_argv`, which
   `ccb` execs) all call it, and `--dangerously-skip-permissions` is in none of them. Limits: a caller's own flags pass
   through verbatim, so `ccb --dangerously-skip-permissions` is the caller's choice, not the recipe's; a caller's own
   `--settings` replaces the recipe's (the recipe adds neither the auto-memory key nor the layer's hooks then, and
   logs it; the layer's skills still load); on Windows the argv keeps the literal name `claude`, found by the daemon's own `PATH`;
   `agent-exec` is Unix only; `"none"` is the bare platform shell and every other kind is refused, never substituted.
2. **A leg starts a fresh conversation unless a reauth or a person resumes one.** The capsule recipes carry no resume
   token. The reauth leg carries `--resume <id>`, handed to the supervisor as `--first-leg-only`, so no later leg or
   watchdog restart carries it. `--continue` is a person's flag (`ccb --continue`, `ccx --continue`), passed through
   `extra` and never added. A fresh session reads its handoff from the `HANDOFF:` line that `sotd comm handoff-line`
   gives `comm-session-start.sh` and `ccx`. Limit: the recipe cannot tell a person's `--resume` in `extra` from a
   reauth's.
3. **`claude` and `ccx` are found by absolute path, by one rule.** On Unix `resolve_claude` and `resolve_ccx` search the
   daemon's `PATH` (absolute entries only; a relative one is skipped) and then `~/.local/bin` (for claude also
   `~/.claude/local`), accept only a regular file the process may execute (a directory named `claude` loses to a later
   real file), return the absolute path, and name every candidate they tried when none runs. `agent_env` puts
   `~/.local/bin` first on a leg's `PATH` once, only when absent, so the leg finds `gh`, the comm launchers and user
   tools a service `PATH` lacks. Limits: Windows resolves by the daemon's `PATH` and has no `ccx`; the check is `access`
   when the leg is built, not at the exec; `ccb` resolves `sotd` by its own ladder (pass 10b).
4. **A leg's environment is built in one place.** `capsule_supervisor_env` gives a row's leg `SOT_SESSION=1`,
   `SOT_SOCKET` (Unix, the daemon's own listener, once it is set), `SOT_WORKSPACE`, `SOT_WORKSPACE_ID`,
   `SOT_WORKSPACE_ROOT`, `SOT_MANUAL` (when the manual's checkout is found), `SOT_AGENT_KIND`, `SOT_COMM_HOME` and
   `SOT_COMM_SELF_FILE` (the row's pinned self file, so a capsule never falls to the shared no-pane slot),
   `SOTD_BIN` (this daemon's own start path, made absolute; a path is not checked to run, only a bare name is looked up), `CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1` and
   `CLAUDE_CODE_DISABLE_AGENT_VIEW=1`, and the `PATH` addition of guarantee 3. The spawn removes
   `NESTING_ENV_VARS_TO_SCRUB` from the daemon's own environment first (`CLAUDECODE` and the other markers that make a
   fresh `claude` think it is nested, and `NO_COLOR`), then applies the row's pairs, then the account's, so an account's
   `CLAUDE_CONFIG_DIR` wins. `sotd agent-exec` scrubs the same list and adds only the `PATH` rule. Limits: comm home and
   self file are absent when no comm home resolves; `SOTD_BIN` is absent only when a bare start name is found nowhere (queue: the runnable check for an explicit path); a
   leg started by `agent-exec` carries none of the `SOT_*` row variables, since it runs in a shell that already has its
   own.
5. **An account is a folder, discovered each time, and a named one never falls back to the default.** `discover_accounts`
   lists `~/.claude`, `~/.codex` and each `~/.claude-auth/<name>` whose name is `[a-z0-9][a-z0-9_-]*`, fresh on every
   `accounts.list`; nothing is declared or cached. `account_env` gives a named Claude account's
   `CLAUDE_CONFIG_DIR` and `SOT_ACCOUNT` and refuses a missing folder (naming the `mkdir` that makes it), an invalid
   name before any path join, a Codex account and a non-agent row. `ensure_account_links` shares an allowlist
   (`SHARED_ENTRIES`: instructions, settings, agents, commands, skills, plugins, output styles, projects, prompt history)
   from the default folder into the account, once, at the spawn, and never the login, the credentials or runtime state;
   an entry already there in any form is left alone, and a sibling spawn's race is tolerated. Where the OS refuses
   symlinks (Windows without the privilege) a folder becomes a junction the daemon proves it can cross and a file a
   one-time copy the account then owns; `CLAUDE.md` there is a copy unless the import stub is switched on (queue:
   CLAUDE-MD-STUB). Limit: the shared `settings.json` must not select a login; nothing here checks it.
6. **Folder trust is recorded only under the declared prefix, and preparation never stops a row.** `sotd trust
   declare <absolute-prefix>` (offline, the installer calls it) writes `[trust] root_prefix` into the daemon's
   `settings.toml` by exclusive creation or one durable replace, and keeps any existing `[trust]` answer
   byte-for-byte. At each Claude spawn `ensure_folder_trusted` records `hasTrustDialogAccepted` for the working
   folder in the effective child's `.claude.json` (the named `CLAUDE_CONFIG_DIR`'s, else the inherited one, else the
   home-level file) only when the OS-resolved folder lies under the OS-resolved prefix; the key keeps the child
   cwd's own spelling (forward slashes on Windows); an entry already accepted is not rewritten; other keys and other
   projects are preserved; this daemon's preparations are serialized and each publishes through a temporary name of its
   own; a file it cannot read or parse is refused, never clobbered. Outside the prefix, with no
   declaration, and on a failed preparation the outcome is logged and the agent still starts. Limits: preparation is
   proved, claude's consumption of it and its no-dialog behavior are the maintainer's done test; a concurrent edit of the file by another process (claude's own) is the writer's stated limit; a `CLAUDE_CONFIG_DIR` that is relative or not UTF-8 records nothing.
7. **A skill and a work-state hook exist only where a leg of the recipe runs, except four onboarding skills.** The
   installer puts every Claude skill but four under `<comm home>/claude-layer/.claude/skills/`. The four, `sot-gh-auth`,
   `sot-install`, `sot-setup` and `sot-statusline-setup` (`CLAUDE_GLOBAL_SKILLS` in `src/sources.jl`, the one list), are
   what a person runs before any row exists, so they install into the global Claude skills folder as before, which every
   named account reaches by the daemon's link; they are never in the layer, and the removal of guarantee 8 never takes
   them. The layer is ready when it holds `sot-session-start`'s
   `SKILL.md`; then `claude_recipe` adds `--add-dir=<layer>` (one token, so the variadic flag cannot take the next
   argument, and an absolute path, since the leg's working folder is the project's) and a `hooks` key, built from `HOOKS`
   in `layer.rs` (`comm-status-working.sh`, `comm-status-blocked.sh`, `comm-status-heartbeat.sh` from the comm folder's
   `bin/`, by quoted absolute path), into the one `--settings` JSON beside the auto-memory key. A layer folder that is
   there but not ready is left out with a warning in the daemon's log, and the leg does not name `/sot-session-start`;
   a box with no layer folder at all (the installer has not run) still names it. A plain `claude` reads none of it; a
   capsule leg, a reauth leg and a `ccb` session read all of it. The end-of-turn mail check (`comm-status-idle.sh` on
   Stop) and the subagent guard serve every joined session, so they stay in the user's own settings, written by the
   installer as before, and gate themselves on a joined identity (messaging's contract); the work-state part of the Stop
   script is not separable from its mail check, so a joined session outside a row still runs it (queue). Limits: `--add-dir`
   also lets the session read and write the layer folder, which the next install overwrites; a caller's own `--settings`
   replaces the recipe's, so that leg carries no work-state hooks (logged; its skills still load); a caller's
   `--setting-sources` without `project` keeps an added folder's skills from loading (logged, and the recipe appends the
   prompt anyway); that Claude Code loads an added folder's `.claude/skills/` is Claude Code's behavior, which no test
   here proves (Claude Code 2.1.308's own text names `--add-dir` as a source of skills, and no session was started to see
   it; the maintainer's done test, as for guarantee 6); the tracked project-local skills of a checkout (`.claude/skills/`)
   load in any session that opens it, row or not, and are not the layer's; the tests prove the argv, the JSON and the
   installed files, not that a shell runs a hook command (the quoting is POSIX and Git Bash's).
8. **An install removes only exact copies of what an earlier installer placed.** The installer, `ShipTools.update_comm`,
   visits every Claude configuration folder (the install's own, `~/.claude`, each `~/.claude-auth/<name>`). In a skills
   folder it removes a skill directory only when its contents hash to a version of that skill the repo shipped
   (`comm/legacy-skills.txt`, 279 versions from `git log` of `agents/claude/`, `comm/adapters/claude/` and
   `.claude/skills/`, made by `scripts/legacy-skill-hashes.sh`; the list is frozen history), warns about any other
   directory of such a name and leaves it; a skills folder that is itself a link is never acted through, and a link inside
   a skill is never followed. From each settings file it removes a hook command only when it equals exactly one an earlier
   installer wrote (`_EARLIER_HOOK_COMMANDS`: the comm scripts under `$HOME/.sot-comm/bin/`, both slash forms) and is not
   one of the two current hooks, one command at a time, dropping a group or an event only when it is left empty; a
   compound command, a command that merely names the path and a script no installer wired are a person's and stay. The
   rules are idempotent and keep no marker. The move and the retirement run only after the layer holds every skill.
   Limits: a version of a skill shipped from a path the history above does not cover is kept (with a warning); a copy
   made on Windows with CRLF checkout hashes differently and is kept; a settings file that does not parse is left alone and
   named in the install's closing warning, which then says the settings were not brought up to date.
9. **The shell tools reach the daemon only through the wire.** Pass 10b settles it, with the launchers and the row CLIs
   below.

## Owns
- The launch recipe: `agent_argv`, `agent_exec_argv`, `claude_resume_argv`, `claude_recipe`, `resolve_claude`,
  `resolve_ccx`, `agent_env`, `NESTING_ENV_VARS_TO_SCRUB`, `account_spawn_env`, `capsule_supervisor_env`,
  `row_self_file`, `own_sotd_bin`, the awareness env, accounts and their links, the folder-trust record and its
  declaration, the auto-memory flag, and `sotd agent-exec` and `accounts.list`.
- The layer: `<comm home>/claude-layer/` (written by the installer from `agents/claude/` and `comm/adapters/claude/`,
  read by the recipe) and the table of hooks in `layer.rs`.
- The non-messaging Claude skills (the four onboarding ones global, the rest the layer's; `julia-repl`, `project-log`, `reauth`, `show-result`, `sitrep`, `sot-gh-auth`,
  `sot-install`, `sot-setup`, `sot-status`, `sot-statusline-setup`, `worktree`) and the Claude launcher `ccb` with its
  `PATH` wrappers (`show-result`, `sot-fe`, `sot-gh-auth`): `claude/`. The Codex launcher `ccx`: `codex/`.
- Pass 10b settles: the row CLIs `spawn/`, the /worktree scripts `worktree/`, the frontend CLI and nav broadcast
  `sot-fe/`, `sot-gh-auth.sh`, and the suites `tests/`.
- Still elsewhere, listed so a reader finds them: the shell daemon client `comm/lib/comm-lib-client.sh`
  (`sot_daemon_endpoint`, `sot_relay_endpoint`, `sot_oneshot_request`, `sot_pty_input`); the hooks' scripts, the Codex
  skills and plugin, and the two messaging skills (`sot-comm`, `sot-session-start`) in `comm/work_state/` and
  `comm/adapters/`: messaging's, installed into the layer by agents' installer stage.

## Calls in
The rows (`rows/spawn/detach.rs` calls `account_spawn_env` and `capsule_supervisor_env`; `rows/run/watchdog.rs`,
`rows/ops/create.rs` and `rows/run/activation.rs` call `agent_argv`; `rows/reauth` calls `claude_resume_argv`,
`account_env`, `discover_accounts` and `claude_config_dir`), the server's op table (`accounts.list`), `main.rs`
(`agent-exec`, `trust declare`, `own_sotd_bin` at start), the installer (`sotd trust declare`; `update_comm`'s adapter
stage for the layer and its retirements), `ccb` (`sotd agent-exec claude`).

## Calls out
Platform (`resource_dir`, durable and no-replace publication, path simplification), messaging (`sot_comm_home`, the
self file's pin, which rows' spawn makes, and the scripts the hooks run), rows (`declared_host`, `app_config_dir`,
`runnable_file`), the OS (`access`, symlinks and junctions). It starts no process of its own: `agent-exec` replaces its
own process with the agent.

## Does not
Run the agent's terminal (capsule), decide when a row starts or ends (rows), read or write mail (messaging), drive the
window (fe-ui's control surface), choose an account's login, or start a Codex row on Windows (`ccx` is bash).

## Platforms
Unix and Windows alike: the recipe's flag shape, accounts, folder trust, the layer and the hooks. Unix only:
absolute resolution of `claude` and `ccx`, `agent-exec`, the `PATH` rule, the codex recipe. Windows only: the
junction and copy fallbacks of guarantee 5, forward-slash spellings of the trust key and `SOT_COMM_HOME`.

## Launchers and the row CLIs (pass 10b settles)
Carried from the charter as it stood, each sentence unproven by this pass:
- `ccb` execs `sotd agent-exec claude`, which carries the recipe; `ccx` chooses no handle (decision 0031): it joins with
  no name and names in codex's first prompt the handle `comm-context.sh` reads back; a join that gives none still
  starts codex and says so (`agents/tests/test-ccx-launch.sh`).
- Scripts run under bash 3.2 and git-bash, as messaging's do (`comm/CLAUDE.md`, `scripts/tests/under-bash.sh`); `ccb`,
  `ccx` and the `sot-fe` and `sot-gh-auth` wrappers start `#!/bin/bash`.
- `sot-gh-auth.sh`, the GitHub device-flow sign-in: every curl call carries `CURL_BOUND` (connect 15 s, under 1000 B/s
  for 60 s) and every gh call that reaches GitHub runs under comm-lib's `sot_bounded` (`GH_BOUND`, 30 s).
- Every endpoint leaves the shell client through `_sot_emit_endpoint` (a whitelist of `unix:`, `pipe:` and `ssh:`); an
  explicit endpoint it refuses is fatal; `sot_relay_endpoint` is what `sotd topology relay-endpoint` answers, takes no
  argument or variable override, and never falls back to the local daemon.
- Every code file directly in this folder installs flat into `~/.sot-comm/bin`, because `comm/bin-folders.txt` lists
  `agents`; `claude/` and `codex/` are never in that list, so their files install only through `src/sources.jl`.
- A cleanup trap follows `comm/CLAUDE.md`'s rule.
- A skill body is a template: Claude Code substitutes `$<digit>` and `$ARGUMENTS` into the text before the model reads
  it, so a skill carries no `$` followed by a digit; the sot-setup skill's launcher script finds its folder by
  `BASH_SOURCE`, not `$0`. No test enforces the rule.
- Folders: `spawn/` (the four row-lifecycle CLIs), `worktree/` (the four /worktree scripts), `sot-fe/` (the frontend
  and REPL CLI, `sot-nav.sh`), `claude/` and `codex/` (installer-shaped, no page), `tests/` (their hermetic suites).

## Files
`claude/`: `bin/ccb`, the wrappers `bin/show-result`, `bin/sot-fe`, `bin/sot-gh-auth`, and the skills. `codex/`:
`bin/ccx`. `spawn/`, `worktree/`, `sot-fe/`, `tests/`: pass 10b's. `sot-gh-auth.sh`. Daemon half:
`rust/backend/src/agents/` (its `CLAUDE.md` points here). Installer half: `src/adapters.jl` (the adapter stages),
`src/skills.jl` (the layer's skills and the one-time move), `src/claude_hooks.jl` (the hook retirement), `src/sources.jl`
(every repo path the installer reads).

## Start here
For a change to how a row and its agent start, `agent_argv` in `rust/backend/src/agents/argv.rs`, then `spawn/comm-spawn.sh`.
For what a session outside a row sees, `rust/backend/src/agents/layer.rs` and `src/skills.jl`. For how a shell tool
reaches the daemon, `sot_daemon_endpoint` in `comm/lib/comm-lib-client.sh`.

## Tests
One test per guarantee, named for it; `cargo test -p sot-backend agents::` runs the unit ones.

| guarantee | test |
|---|---|
| 1 | `argv::tests::every_claude_leg_runs_auto_mode_and_never_skips_permissions` |
| 2 | `argv::tests::agent_argv_claude_starts_a_fresh_conversation`, `a_reauth_leg_resumes_by_id_and_never_continues`, `agent_exec_argv_claude_never_adds_continue`; the supervisor's stripping of a resume on later legs, `rust/log/src/supervisor/transitions_tests.rs` |
| 3 | `argv::tests::resolve_claude_*`, `resolve_ccx_*`; `env::tests::agent_env_prepends_local_bin_to_path_once`; `tests/agent_exec.rs` |
| 4 | `env::tests::capsule_supervisor_env_*`, `nesting_env_scrub_list_matches_ccb`; `tests/agent_exec.rs` |
| 5 | `accounts::tests::*`, `account_links::tests::*` |
| 6 | `folder_trust::tests::*` (`two_preparations_never_share_a_temporary_name`), `trust_declaration::tests::*`, `env::trust_declaration_controls::*`, `env::trust_scope_controls::*`, `env::trust_config_controls::*`; `tests/trust_declare.rs`, `tests/trust_premises.rs` |
| 7 | `argv::tests::the_layer_rides_the_recipe_and_a_plain_claude_has_none`, `a_layer_without_the_bootstrap_skill_is_left_out_and_the_prompt_goes_with_it`, `the_layer_reaches_the_capsule_leg_the_reauth_leg_and_agent_exec`, `layer::tests::*`; `tests/agent_exec.rs::agent_exec_claude_carries_the_layer_when_installed`; `test/layer_tests.jl`, `test/protect_tests.jl` ("the mail check and the subagent guard stay in the user's settings"). A leg that carries the layer was proved at its argv, not by running Claude Code |
| 8 | `test/protect_tests.jl` (a person's skill, a symlinked skills folder, every configuration folder, a person's hook), `test/claude_hooks_tests.jl` (the settings retirement) |
| 9 | pass 10b |

## Queue
- A caller's own `--settings` carries no work-state hooks; merging the recipe's hooks into a JSON value the caller gives
  (a file path would need reading) is the fix.
- The Stop script `comm-status-idle.sh` does the mail check and the work-state stamping, audits and nudges in one
  574-line flow, and every joined session runs it. Splitting the mail check from the work-state part is messaging's and
  rows' pass; the layer then carries the work-state half.
- `SOTD_BIN`: check that an explicit start path runs, as a bare name is checked (`env.rs`, `sotd_bin_value`).
- The Windows skill-folder hash: a CRLF checkout's copies differ from the list and are kept.
- The auto-memory flag (`memory.rs`, `--settings` key `autoMemoryDirectory`) has no guarantee of its own in this page;
  it is owned policy of the recipe and is proved by `memory::tests::*`.
- A shell test of the hook command's quoting under Git Bash and macOS's shell (hosted run).
- 5 REMOTE-TOOL: a feature; left.
- 51 CLAUDE-MD-STUB: needs a Windows box with a logged-in account and a real `claude -p`; left.
- Codex: its skills and plugin still install into the global `$CODEX_HOME` and `~/.agents`, outside any row. The same
  question as guarantee 7, for Codex; a pass decides it with the Codex plugin mechanism.
