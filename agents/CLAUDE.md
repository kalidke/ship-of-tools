# agents: the edge with agent programs (contract)

Ship of Tools meets an agent program (Claude Code, Codex) at one edge. The daemon starts the program from one launch
recipe (argv, environment, account, folder trust), and the program drives Ship of Tools only through the daemon's wire,
using the tools and skills installed from the repository. Two halves: the daemon's, in `rust/backend/src/agents/` (the
launch recipe: `argv.rs`, `env.rs`, `awareness.rs`, `accounts.rs`, `account_links.rs`, `folder_trust.rs`,
`trust_declaration.rs`, `memory.rs`, `layer.rs` and the ops in `ops.rs`), and the shell's, in this folder (the launchers,
the row CLIs, the skills).

## Guarantees
Each states what the rest may rely on, the mechanism it rests on and the limit it has. The Tests section holds the test
of each.

1. **One recipe builds every Claude leg, and every leg runs `--permission-mode auto`.** `claude_recipe` (`argv.rs`) is
   the one builder of the flag shape: `claude --permission-mode auto [--settings <json>] <caller's flags>
   /sot-session-start`, with the layer's `--add-dir` in front when the layer is installed (guarantee 7). The capsule
   leg (`claude_argv`), the reauth leg (`claude_resume_argv`) and `sotd agent-exec claude` (`agent_exec_argv`, which
   `ccb` execs) all call it, and `--dangerously-skip-permissions` is in none of them. Limits: a caller's own flags pass
   through verbatim, so `ccb --dangerously-skip-permissions` is the caller's choice, not the recipe's; a caller's own
   `--settings` stays the one, and the recipe adds no auto-memory key to it. The shape rule: the layer's hooks are merged in
   (guarantee 7) only when the value is a JSON object whose `hooks`, if it has any, is an object, and every event the
   recipe also names is an array there; a recipe hook whose command the caller's event already carries is not added
   again, and the recipe's other groups follow the caller's. Anything else (invalid JSON, a root that is not an object,
   `"hooks": null` or another non-object, an event that is not an array, a file path, which would need reading, or more
   than one `--settings`) is passed as given, with no hooks, and the recipe logs "a caller's own --settings is not the
   only one, or not a JSON object whose hooks are an object of arrays" (the layer's skills still load);
   on Windows the argv keeps the literal name `claude`, found by the daemon's own `PATH`;
   `agent-exec` is Unix only; `"none"` is the bare platform shell and every other kind is refused, never substituted.
2. **A leg starts a fresh conversation unless a reauth or a person resumes one.** The capsule recipes carry no resume
   token. The reauth leg carries `--resume <id>`, handed to the supervisor as `--first-run-only`, so no later leg or
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
   when the leg is built, not at the exec; `ccb` resolves `sotd` by its own ladder (guarantee 10).
4. **A leg's environment is built in one place.** `capsule_supervisor_env` gives a row's leg `SOT_SESSION=1`,
   `SOT_SOCKET` (Unix, the daemon's own listener, once it is set), `SOT_WORKSPACE`, `SOT_WORKSPACE_ID`,
   `SOT_WORKSPACE_ROOT`, `SOT_MANUAL` (when the manual's checkout is found), `SOT_AGENT_KIND`, `SOT_COMM_HOME` and
   `SOT_COMM_SELF_FILE` (the row's pinned self file, so a capsule never falls to the shared no-pane slot),
   `SOTD_BIN` (this daemon's own start path, made absolute; it is checked to be a regular file, executable on Unix, and is not run; a bare name is looked up on `PATH` the same way), `CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1` and
   `CLAUDE_CODE_DISABLE_AGENT_VIEW=1`, and the `PATH` addition of guarantee 3. The spawn removes
   `NESTING_ENV_VARS_TO_SCRUB` from the daemon's own environment first (`CLAUDECODE` and the other markers that make a
   fresh `claude` think it is nested, and `NO_COLOR`), then applies the row's pairs, then the account's, so an account's
   `CLAUDE_CONFIG_DIR` wins. `sotd agent-exec` scrubs the same list and adds only the `PATH` rule. Limits: comm home and
   self file are absent when no comm home resolves; `SOTD_BIN` is always set, and empty when the start path names no such file or a bare start name is found nowhere, so the leg never inherits the daemon's own value over it, and the session's shell, which reads an empty value as unset, finds `sotd` by its own ladder (an executable that does not work, a missing interpreter included, passes the check); a
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
   CLAUDE-MD-STUB). A session that starts a row gives it its own account unless told which (the maintainer,
   2026-10-27): with no `--account`, `comm-spawn.sh` names for a Claude row the `.claude-auth/<name>` folder its caller's
   `CLAUDE_CONFIG_DIR` names (either slash), else the caller's `SOT_ACCOUNT` when that variable is unset; a caller on
   the default folder, on a folder of another shape or outside any account keeps the default, `--account` always wins
   (`--account default` asks for the default), and a Codex or bash row names none. Limits: the shared `settings.json`
   must not select a login, and nothing here checks it; a spawn on another computer names the caller's account there,
   and that daemon refuses it if the folder is missing.
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
   also lets the session read and write the layer folder, which the next install overwrites; a caller's own `--settings` gains the hooks by guarantee 1's shape rule, and one the rule declines is left as given,
   so that leg carries no work-state hooks (logged; its skills still load); a caller's
   `--setting-sources` without `project` keeps an added folder's skills from loading (logged, and the recipe appends the
   prompt anyway); that Claude Code loads an added folder's `.claude/skills/` is Claude Code's behavior, which no test
   here proves (Claude Code 2.1.308's own text names `--add-dir` as a source of skills, and no session was started to see
   it; the maintainer's done test, as for guarantee 6); the tracked project-local skills of a checkout (`.claude/skills/`)
   load in any session that opens it, row or not, and are not the layer's; the tests prove the argv, the JSON and the
   installed files, and that a shell runs each hook command the leg is given from a comm folder whose name holds a space
   and a quote (`tests/test-hook-commands.sh`, under whatever bash runs it: macOS's 3.2 and Linux's 3.2.57 on the hosted runs;
   `agent-exec` is Unix only, so Git Bash, whose quoting is the same POSIX form, is not run).
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
9. **The shell tools of this folder reach the daemon only through the wire.** The tools are the launchers `ccb` and
   `ccx`, the row CLIs in `spawn/`, `worktree/` and `sot-fe/`, `sot-gh-auth.sh` and the `PATH` wrappers of `claude/bin/`.
   Their route is one of three, and none opens a file the daemon owns. (a) A framed op over an endpoint, through
   comm-lib's `sot_oneshot_request`, which says hello first (the daemon admits nothing before it): `sot-fe`
   (`fe.command.send`, `repl.execute`, `repl.run_file`, `repl.interrupt`, `pty.input`, `pty.screen`,
   `workspace.reauth`, `version.query`, `workspace.list`), `comm-spawn.sh` and `comm-despawn.sh` (`workspace.create`,
   `workspace.list`, `workspace.destroy`, `version.query`), `comm-bootstrap.sh` and `comm-probe.sh` (`workspace.list`,
   `workspace.create`, `pty.input`), `sot-nav.sh` (`agent.send`, to the hub's relay endpoint). (b) A `sotd` subcommand:
   `sotd comm VERB` through `sot_comm` (the registry, unread counts, identity, a handoff line), `sotd agent-exec`
   (`ccb`), `sotd session-socket-path` and `sotd topology relay-endpoint` (where the daemon is). (c) The tool's own work
   in its own folder or in git (the /worktree scripts). The endpoint is the one thing a tool finds: an explicit one,
   `SOT_SOCKET`, or the path `sotd session-socket-path` names when a test finds a socket there (the type, not a
   listener that answers), and every value leaves comm-lib through `_sot_emit_endpoint`, a whitelist of `unix:`, `pipe:`
   and `ssh:` (an explicit value it refuses is fatal; the hub's `sot_relay_endpoint` takes no argument or variable and
   never falls back to the local daemon; messaging's `comm/tests/test-endpoint-gate.sh`). In this folder the worktree
   scripts read the registry as `sotd comm registry read` answers, the probe counts its inbox with `sotd comm unread`,
   and the spawn leaves the inbox to the daemon's first append. Limits: a tool keeps files of its own in the comm folder
   (`bin/`, written by the installer; `probe/`; `gh-device-auth.json`), and `sot-fe version` prints the install's
   `VERSION` file; `ccx` reads and appends to Codex's own `config.toml`, which is Codex's and not the daemon's; the
   tools' hello is checked by the server (messaging's and the server's contracts), not here. **Shipped tools outside this
   folder's ownership still open daemon-owned paths, and this guarantee does not cover them:** the work-state hooks
   (`comm-status.sh`, `comm-status-heartbeat.sh`, `comm-status-idle.sh`) and the turn auditor, rows' and messaging's, test
   the registry and the inbox and keep turn marks and a signature under the comm folder's `state/` (queued under rows:
   WORK-STATE-HOOKS-READ-FILES); the Windows launcher's stop helper reads `held.json` (queued under distribution:
   WIN-LAUNCHER-HELD-JSON). Test: `tests/test-tools-wire-only.sh` (the registry and inbox folders the tools are told of
   do not exist, and a stand-in `sotd` answers the registry and the unread count; worktree status and sync and the
   probe's responder, through one delivered mail and its report, give the daemon's answers and leave the comm folder's
   whole tree as it was), `tests/test-spawn-remote-no-local-row.sh` (a local spawn writes its row through the daemon
   and no inbox file), and `tests/test-sot-fe-endpoint-once.sh` (a `sot-fe` run asks `sotd` for the socket once,
   whichever verb). The other tools are read, not run, by it: their routes are the ones listed.
10. **`ccb` carries no recipe of its own; `ccx` builds Codex's argv itself.** `ccb` finds `sotd` by `$SOTD_BIN` when it is
    executable, else `PATH`, else `~/.local/share/sot/bin/sotd`, else `~/.local/bin/sotd`, and execs `sotd agent-exec
    claude` with the caller's flags in order (guarantee 1), or says `ccb: sotd not found` and exits 1. `ccx` has no
    daemon recipe behind it: it builds the Codex argv itself (`codex [resume <id>]
    --dangerously-bypass-approvals-and-sandbox --dangerously-bypass-hook-trust <prompt>`). It chooses no handle
    (decision 0031): it joins with no name and names, in Codex's first prompt, the handle `comm-context.sh` reads back; a
    join that gives none still starts Codex, with a prompt that names none and one stderr line saying so; it starts a
    fresh conversation unless `--continue` or `--resume` is given (`--fresh` wins over `--continue`). Limits: `ccx` runs
    Codex with its approvals and its sandbox bypassed, and is bash: it has no Windows form. Test:
    `tests/test-ccb-agent-exec.sh` (the ladder and the order), `tests/test-ccx-launch.sh`,
    `tests/test-ccx-resume-default.sh`.
11. **The tools run under macOS's bash 3.2 and Git Bash.** They use no bash 4 feature, as messaging's scripts do
    (`comm/CLAUDE.md`). Limit: the proof is the hosted run of `scripts/tests/under-bash.sh` on macOS and on a built bash
    3.2.57 on Linux (a new suite joins by its name); it runs a suite's own body in the newer shell and puts bash 3.2
    first on `PATH` for the scripts the suite starts, so the suites' code is not proved under 3.2; no hosted job runs the
    agents suites under Git Bash, and a local run is under bash 5 and proves nothing of it.
12. **`sot-gh-auth.sh`, the GitHub device-flow sign-in, waits at most a stated time.** Every `curl` call carries
    `CURL_BOUND` (connect 15 s, a call 60 s in all, under 1000 B/s for 60 s), and every `gh` call that reaches GitHub
    runs under comm-lib's `sot_bounded` (`GH_BOUND`, 30 s) and names the expiry on the script's own stderr. The poll's
    expiry counts the sleeps and the time each request took, so a slow GitHub cannot stretch the code's life. Limit:
    `gh auth setup-git`, which writes git's configuration and reaches no network, runs unbounded. Test:
    `tests/test-crlf-gh-auth.sh` ("every curl call carries the stall bound"; "at the script's 30 s bound"; "a slow
    GitHub ends the poll at the code's expiry, by elapsed time").
13. **Where the tools install is fixed by one list.** Every code file directly in each folder `comm/bin-folders.txt`
    lists (`agents`, `agents/sot-fe`, `agents/spawn`, `agents/worktree`) installs flat into the comm folder's `bin/`,
    except that `sot-fe`'s four parts (`sot-fe-request.sh`, `sot-fe-repl.sh`, `sot-fe-row.sh`, `sot-fe-version.sh`) are
    embedded in the installed `sot-fe` and are no files of their own (`src/comm_bin.jl`); nothing in `claude/` or
    `codex/` installs there (the launchers and the `PATH` wrappers reach `~/.local/bin` through the installer's own
    stage, `src/sources.jl`), and the wrapper `claude/bin/sot-fe` never replaces the tool `sot-fe/sot-fe`. Test:
    `test/install_tests.jl` ("the agents tools install flat from the four listed folders"), over the installer's own
    file list.
14. **No tool leaves a file in the temporary folders, by messaging's rule and with its limits.** On success, on failure,
    killed by a catchable signal and ended twice, no script of this folder leaves a file in `$TMPDIR` or
    `$XDG_RUNTIME_DIR`, by the cleanup-trap rule of `comm/CLAUDE.md` (a shell running `comm-spawn.sh` that stops on
    Ctrl-C says its rollback's outcome). Limits, the rule's own (`comm/lib/comm-lib-base.sh`, cleanup traps): SIGKILL
    runs no trap, so a request killed that way leaves its scratch folder, and a signal that arrives inside the handler
    itself can leave one. Test: `tests/test-temp-files.sh`, whose kill rows stop at marked points, not every one.
15. **A skill body is a template, so it carries no `$` and a digit.** Claude Code substitutes `$<digit>` and
    `$ARGUMENTS` into the text before the model reads it; a skill file of the layer or of the checkout carries no `$0`,
    `$1` or `${1}`. Limit: the rule keeps a skill from being spoiled by a substitution, not from being wrong, and no
    test runs a skill's script or Claude Code. Check (a lint over the files, which a skill with `$1` fails):
    `tests/test-skill-templates.sh`.

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
- The row CLIs `spawn/`, the /worktree scripts `worktree/`, the frontend CLI and nav broadcast `sot-fe/`,
  `sot-gh-auth.sh`, and the suites `tests/`.
- `sot-fe docs <workspace> <absolute-page>` carries an explicit entry reply scope through `fe.command docs`:
  ordinary by default, `--direct` for the named row, or `--direct-all` for all rows (the list pages). The two flags are
  exclusive and accepted only for docs. The daemon resolves the row to its canonical id and refuses a row-scoped
  page outside its project folder; the CLI never infers authority from a document. Navigation to another HTML
  document is ordinary, and the page reads `states.reply_scope` before enabling scoped controls. Landing A carries
  the scope only; replies still use the window's confirmation bar. `tests/test-sot-fe-paths.sh` records the scope in
  the actual request; `tests/test-sot-fe-goto.sh` covers invalid flag combinations.
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

## Files
`claude/`: `bin/ccb`, the wrappers `bin/show-result`, `bin/sot-fe`, `bin/sot-gh-auth`, and the skills. `codex/`:
`bin/ccx`. `spawn/` (the four row-lifecycle CLIs), `worktree/` (the four /worktree scripts), `sot-fe/` (the frontend and
REPL CLI and `sot-nav.sh`), `tests/` (their hermetic suites), `sot-gh-auth.sh`. Daemon half:
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
| 2 | `argv::tests::agent_argv_claude_starts_a_fresh_conversation`, `a_reauth_leg_resumes_by_id_and_never_continues`, `agent_exec_argv_claude_never_adds_continue`; the supervisor's stripping of a resume on later legs, `rust/capsule/tests/supervisor/spawn.rs`: `first_run_only_keeps_a_token_on_the_first_leg_and_strips_it_after_a_producer_ran` |
| 3 | `argv::tests::resolve_claude_*`, `resolve_ccx_*`; `env::tests::agent_env_prepends_local_bin_to_path_once`; `tests/agent_exec.rs` |
| 4 | `env::tests::capsule_supervisor_env_*`, `nesting_env_scrub_list_matches_ccb`, `sotd_bin_value_*`, `the_leg_never_inherits_a_sotd_bin_its_own_start_path_rejected`; `tests/agent_exec.rs` |
| 5 | `accounts::tests::*`, `account_links::tests::*`; `tests/test-spawn-capsule-workspace.sh` ("no --account: a claude row gets the spawning session's account") |
| 6 | `folder_trust::tests::*` (`two_preparations_never_share_a_temporary_name`), `trust_declaration::tests::*`, `env::trust_declaration_controls::*`, `env::trust_scope_controls::*`, `env::trust_config_controls::*`; `tests/trust_declare.rs`, `tests/trust_premises.rs` |
| 7 | `argv::tests::the_layer_rides_the_recipe_and_a_plain_claude_has_none`, `a_layer_without_the_bootstrap_skill_is_left_out_and_the_prompt_goes_with_it`, `the_layer_reaches_the_capsule_leg_the_reauth_leg_and_agent_exec`, `a_callers_settings_json_is_merged_with_the_recipes_hooks`, `layer::tests::*`;
`tests/agent_exec.rs::agent_exec_claude_carries_the_layer_when_installed`; `tests/test-hook-commands.sh`; `a_recipe_hook_the_caller_already_has_is_not_added_twice`, `a_callers_settings_of_another_shape_is_passed_as_given`; `test/layer_tests.jl`, `test/protect_tests.jl` ("the mail check and the subagent guard stay in the user's settings"). A leg that carries the layer was proved at its argv, not by running Claude Code |
| 8 | `test/protect_tests.jl` (a person's skill, a symlinked skills folder, every configuration folder, a person's hook), `test/claude_hooks_tests.jl` (the settings retirement) |
| 9 | `tests/test-tools-wire-only.sh`, `tests/test-spawn-remote-no-local-row.sh`, `tests/test-sot-fe-endpoint-once.sh` |
| 10 | `tests/test-ccb-agent-exec.sh`, `tests/test-ccx-launch.sh`, `tests/test-ccx-resume-default.sh` |
| 11 | the hosted run of `scripts/tests/under-bash.sh` |
| 12 | `tests/test-crlf-gh-auth.sh` |
| 13 | `test/install_tests.jl` (the agents testset) |
| 14 | `tests/test-temp-files.sh` |
| 15 | `tests/test-skill-templates.sh` (a lint) |

## Queue
- The Stop script `comm-status-idle.sh` does the mail check and the work-state stamping, audits and nudges in one
  574-line flow, and every joined session runs it. Splitting the mail check from the work-state part is messaging's and
  rows' pass; the layer then carries the work-state half.
- The Windows skill-folder hash: a CRLF checkout's copies differ from the list and are kept.
- The auto-memory flag (`memory.rs`, `--settings` key `autoMemoryDirectory`) has no guarantee of its own in this page;
  it is owned policy of the recipe and is proved by `memory::tests::*`.
- 5 REMOTE-TOOL: a feature; left.
- 51 CLAUDE-MD-STUB: needs a Windows box with a logged-in account and a real `claude -p`; left.
- Codex: its skills and plugin still install into the global `$CODEX_HOME` and `~/.agents`, outside any row. The same
  question as guarantee 7, for Codex; a pass decides it with the Codex plugin mechanism.
