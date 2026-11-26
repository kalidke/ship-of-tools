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
   `SOT_SELECTION_FILE` (the row's `selection.json` in its state folder, on Windows with forward slashes and no verbatim `\\?\` prefix, for a Claude and a Codex leg alike; the hook that reads it is `agents/sot-selection-hook.sh`, guarantee 7),
   `SOTD_BIN` (this daemon's own start path, made absolute; it is checked to be a regular file, executable on Unix, and is not run; a bare name is looked up on `PATH` the same way), `CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1` and
   `CLAUDE_CODE_DISABLE_AGENT_VIEW=1`, for a Claude leg alone `CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1` (Claude Code's
   classic renderer on the main screen, over the person's `tui` setting, so the window's scroll and selection of the
   agent pane apply; a Codex and a terminal-only leg get none), and the `PATH` addition of guarantee 3. Every row launch
   (create, attach, boot resume, reauth, the trust restart, the watchdog's restart) builds its leg's pairs here, through
   `rows::spawn::detach::spawn_detached_supervisor`, the one place a supervisor is launched (checked by search, not by a
   test). The spawn removes
   `NESTING_ENV_VARS_TO_SCRUB` from the daemon's own environment first (`CLAUDECODE` and the other markers that make a
   fresh `claude` think it is nested, `NO_COLOR`, and `SOT_SELECTION_FILE`, so a nested row or a `ccb` session never reads an outer row's selection; `ccx` unsets it itself unless started with `--capsule`, which is a Codex row's own leg and keeps it), then applies the row's pairs, then the account's, so an account's
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
   one-time copy the account then owns; `CLAUDE.md` there is a copy unless the import stub is switched on (docs/defects.md: CLAUDE-MD-STUB). A session that starts a row gives it its own account unless told which (the maintainer,
   2026-10-27): with no `--account`, `comm-spawn.sh` names for a Claude row the `.claude-auth/<name>` folder its caller's
   `CLAUDE_CONFIG_DIR` names (either slash), else the caller's `SOT_ACCOUNT` when that variable is unset; a caller on
   the default folder, on a folder of another shape or outside any account keeps the default, `--account` always wins
   (`--account default` asks for the default), and a Codex or bash row names none. Limits: the shared `settings.json`
   must not select a login, and nothing here checks it; a spawn on another computer names the caller's account there,
   and that daemon refuses it if the folder is missing.
6. **Folder trust is recorded only under a declaration, and preparation never stops a row.** `sotd trust
   declare <absolute-prefix>` (offline, the installer calls it) writes `[trust] root_prefix` into the daemon's
   `settings.toml` by exclusive creation or one durable replace, and keeps any existing `[trust]` answer
   byte-for-byte. At each Claude spawn `ensure_folder_trusted` records `hasTrustDialogAccepted` for the working
   folder in the effective child's `.claude.json` (the named `CLAUDE_CONFIG_DIR`'s, else the inherited one, else the
   home-level file) only when the OS-resolved folder lies under the OS-resolved prefix; the key keeps the child
   cwd's own spelling (forward slashes on Windows); an entry already accepted is not rewritten. The same spawn records
   the layer folder the leg adds with `--add-dir` (guarantee 7) by `ensure_layer_trusted`, when the layer is ready
   (`layer_under`) and a declaration is read, wherever the layer lies, under the spelling the leg's `--add-dir` carries (a link is never resolved: a record under a link's spelling and one under its target's each suppressed the dialog): it is in the comm home, outside any prefix, and
   Claude Code 2.1.326 stops at "Adding a new directory" for an added folder that has no record of its own or a
   parent's (measured under a throwaway configuration: no record, the dialog; a record for the layer, spelled by its
   path or by a link to it, none). With no declaration, or one that is relative or unreadable, neither folder is
   recorded. Other keys and other
   projects are preserved; this daemon's preparations are serialized and each publishes through a temporary name of its
   own; a file it cannot read or parse is refused, never clobbered. Every preparation is one log line
   per folder (the row's, then the layer's, which says `layer=true`) naming its outcome: recorded, already trusted and
   outside at info, each with the trust file; no declaration at warn, once, with the declaration file; a failure at
   warn, with its error; the agent starts whatever it says. For rows'
   trust restart, `folder_trust::under_declared_prefix` answers that same scope test (one private helper both
   call), and every Claude preparation marks its trust file with an instant, in memory, that `claude_prepared_within`
   reads. Limits: preparation is
   proved, claude's consumption of it and its no-dialog behavior are the maintainer's done test (the added-folder
   behavior above was measured on a scratch configuration with no login, so it is not a row's run);
   the trust restart (rows' `run/trust_restart.rs`) recognizes the row's own trust dialog (`Quick safety check:`) and not
   the added-folder one, so a layer record lost to Claude Code's own save (docs/defects.md: REAUTH-TRUST-LOST) leaves
   that dialog to a person; a concurrent edit of the file by another process (claude's own) is the writer's stated limit; a `CLAUDE_CONFIG_DIR` that is relative or not UTF-8 records nothing.
7. **Plain Claude loads the four onboarding skills. Claude rows and `ccb` add the row layer. Plain Codex with its default settings leaves the row skills to `ccx`.** The
   installer puts every Claude skill but four under `<comm home>/claude-layer/.claude/skills/`. The four, `sot-gh-auth`,
   `sot-install`, `sot-setup` and `sot-statusline-setup` (`CLAUDE_GLOBAL_SKILLS` in `src/sources.jl`, the one list), are
   what a person runs before any row exists, so they install into the global Claude skills folder as before, which every
   named account reaches by the daemon's link; they are never in the layer, and the removal of guarantee 8 never takes
   them. The layer is ready when it holds `sot-session-start`'s
   `SKILL.md`; then `claude_recipe` adds `--add-dir=<layer>` (one token, so the variadic flag cannot take the next
   argument, and an absolute path, since the leg's working folder is the project's) and a `hooks` key, built from `HOOKS`
   in `layer.rs` (`comm-status-working.sh`, `sot-selection-hook.sh` (also on UserPromptSubmit: it reads its whole input first, as the host writes the prompt to it and a long prompt would otherwise meet a closed pipe, then prints the file `SOT_SELECTION_FILE` names when that is set and names a readable regular file, and otherwise nothing, always exiting 0; tested under bash 3.2 and bash 5 on Linux and macOS, and by reading, not running, under Git Bash; its command is `[ -f '<path>' ] || exec cat > /dev/null 2>&1; '<path>'`, so a comm folder an older release's install pruned of the script is a silent no-op that reads its whole input too, not an error row on every prompt; it is this folder's script, and the installer lays it flat in the comm folder's `bin/` with the others), `comm-status-blocked.sh`, `comm-status-heartbeat.sh` and, on the SessionStart
   sources `clear` and `compact`, the session-start hook script (messaging's), which runs the comm session start read-only
   (`--again`: it joins and writes nothing, so the row's work-state is kept) so a session that lost its context is told
   its handle, its handoff file and any question its row kept, and stands down on `SOT_COMM_HOOKS=off`; from the comm folder's
   `bin/`, by quoted absolute path; a leg's first start and a leg started with `--resume <id>` run the start through the
   bootstrap prompt; `/resume` typed in a live row is the `resume` source, which the recipe does not carry the hook for
   and no test covers), into the one `--settings` JSON beside the auto-memory key. A layer folder that is
   there but not ready is left out with a warning in the daemon's log, and the leg does not name `/sot-session-start`;
   a box with no layer folder at all (the installer has not run) still names it. A plain `claude` reads none of it; a
   capsule leg, a reauth leg and a `ccb` session read all of it. The end-of-turn mail check (`comm-status-idle.sh` on
   Stop) and the subagent guard serve every joined session, so they stay in the user's own settings, written by the
   installer as before, and gate themselves on a joined identity (messaging's contract); the work-state part of the Stop
   script is not separable from its mail check, so a joined session outside a row still runs it (docs/defects.md: WORK-STATE-HOOKS-READ-FILES). Limits: `--add-dir`
   also lets the session read and write the layer folder, which the next install overwrites, and Claude Code asks whether to trust an added folder that has no trust record, which a leg cannot answer, so guarantee 6 records it (with no declaration the dialog stands); a caller's own `--settings` gains the hooks by guarantee 1's shape rule, and one the rule declines is left as given,
   so that leg carries no work-state hooks (logged; its skills still load); a caller's
   `--setting-sources` without `project` keeps an added folder's skills from loading (logged, and the recipe appends the
   prompt anyway); that Claude Code loads an added folder's `.claude/skills/` is Claude Code's behavior, which no test
   here proves (Claude Code 2.1.308's own text names `--add-dir` as a source of skills, and no session was started to see
   it; the maintainer's done test, as for guarantee 6); the tracked project-local skills of a checkout (`.claude/skills/`)
   load in any session that opens it, row or not, and are not the layer's; the tests prove the argv, the JSON and the
   installed files, and that a shell runs each hook command the leg is given from a comm folder whose name holds a space
   and a quote (`tests/test-hook-commands.sh`, under whatever bash runs it: macOS's 3.2 and Linux's 3.2.57 on the hosted runs;
   `agent-exec` is Unix only, so Git Bash, whose quoting is the same POSIX form, is not run).
   Codex loads the three messaging skills and the work-state plugin globally. The row plugin
   `sot-row@sot-local` carries the Claude sources named in `codex/shared-skills.txt` and unfiltered
   `SessionStart` and `UserPromptSubmit` hooks. The selection command on prompt submit reads the whole prompt and runs the installed selection script through
   the runtime comm home when a row selection path and the script are present.
   Both events also run the binding hook. The winning supervisor clears the row's `codex-thread` before every Codex
   leg. Only the owning leg gets `SOT_CODEX_THREAD_FILE`. The hook checks the process ancestry and atomically replaces
   the file with the input's UUID `session_id`. A missing or malformed id leaves the previous file intact. If creating,
   writing or renaming the temporary file fails, the hook cleans up and exits silently with status zero. A TERM before
   rename does the same. SIGKILL can leave a temporary file. The previous id stays at the binding path until a rename
   completes. A child hook carrying the parent's session id therefore keeps the root binding. These inputs have been
   checked with fixtures and the hook schema. A live row still needs the checks in `CODEX-ROOT-THREAD-HOOKS`. Between a
   conversation switch and the next start or submit hook the file can name the conversation just left, possibly already unloaded. Mail stays
   in the inbox for the next poll. Messaging's wake reads the file to ask the conversation it names to poll
   (messaging's contract, Waking; the file's name is `env::CODEX_THREAD_FILE`).
   A second `SessionStart` command restores the owning row's context after `clear`, `compact` and `resume`. It runs
   the shared `comm-session-start.sh --again` and returns its successful lines once in `hookSpecificOutput.additionalContext`.
   These lines name the pinned handle, its handoff when present and any open question. The hook reads the existing
   registration and leaves the question and inbox cursor in place. Fresh startup uses the launcher's prompt. Fork and
   subagent starts, nested launches, a foreign capsule, disabled hooks and bootstrap failures receive no recovery text.
   The source names and output shape come from the installed Codex 0.171.0 schemas. Live delivery awaits the check
   in `CODEX-ROOT-THREAD-HOOKS`. The binding command runs independently at the same event.
   The installer publishes the plugin in the versioned Codex cache on Unix, replacing the product's whole folder.
   Windows skips this publish until it
   has a Codex row launcher. With no cached plugin, Codex 0.171.0 rendered the prompt with the ccx override,
   zero row skills and no warning. A retry restores a displaced tree.
   Only this publish takes a nonblocking lock. A dead holder releases it. A live holder makes a competing install
   skip this publish with a message and finish its other stages. Any other lock failure reports the path or system
   error and ends the install as INCOMPLETE in about three seconds in the measured cases, without removing personal files.
   The installer leaves the row plugin out of the
   marketplace and the personal configuration. Fresh, resume and continue launches by `ccx` enable it even over
   a personal false setting. Codex 0.171.0 listed zero, six, then zero row skills before, during and after the
   override with a default configuration. An explicit personal true setting also enables it in plain Codex. The plugin listing omitted it.
   Where the installed command supports `--no-daemon`, `ccx` starts the row in its own process even with a shared
   background server running. Discovery from a plain request in a live row awaits a check after release (`CODEX-ROW-SKILL-DISCOVERY`).
   Tests: `test/codex_tests.jl` (installation, contention and recovery), `tests/test-ccx-launch.sh` and
   `tests/test-ccx-resume-default.sh` (launch argv), and `tests/test-skill-templates.sh` (manual's skill list).
8. **An install removes only exact copies of what an earlier installer placed.** The installer, `ShipTools.update_comm`,
   visits every Claude configuration folder (the install's own, `~/.claude`, each `~/.claude-auth/<name>`). In a skills
   folder it removes a skill directory only when its contents hash to a version of that skill the repo shipped
   (`comm/legacy-skills.txt`, every version in `git log` of `agents/claude/`, `comm/adapters/claude/` and
   `.claude/skills/`, made by `scripts/legacy-skill-hashes.sh`, so a landing that changes a skill's text regenerates the
   list after its commits, and `test/protect_tests.jl`, which copies the current skill, fails until it does for every
   skill the retire reads; the onboarding skills, which it never reads, can lag without effect), warns about any other
   directory of such a name and leaves it; a skills folder that is itself a link is never acted through, and a link inside
   a skill is never followed. From each settings file it removes a hook command only when it equals exactly one an earlier
   installer wrote (`_EARLIER_HOOK_COMMANDS`: the comm scripts under `$HOME/.sot-comm/bin/`, both slash forms) and is not
   one of the two current hooks, one command at a time, dropping a group or an event only when it is left empty; a
   compound command, a command that merely names the path and a script no installer wired are a person's and stay. The
   rules are idempotent and keep no marker. The move and the retirement run only after the layer holds every skill.
   Limits: a version of a skill shipped from a path the history above does not cover is kept (with a warning); a copy
   made on Windows with CRLF checkout hashes differently and is kept; a settings file that does not parse is left alone and
   named in the install's closing warning, which then says the settings were not brought up to date.
   Before its first write, the comm install checks every shared PATH command in guarantee 13 and, on Windows, its
   `.cmd` shim. It reports every occupied path it cannot recognize, leaves the comm install and its version stamp
   intact, and tells the person to move or rename the entry and retry. It checks each name again before publishing
   it in case the path changed after the first check. A regular wrapper can match the shipped text
   or an earlier digest in `comm/launcher-hashes.txt`, with uniform LF or CRLF. Mixed endings, an edited or
   unrecorded version, a link (including a dangling one), or a folder requires moving the entry. Windows shims
   must match their exact shipped text. This branch was checked in code and has yet to run on Windows.
   After a successful replacement, cleanup removes partial stages of a dead local process on Unix and removes a
   shared command's kept-aside copy only if its name and bytes match. The adapter launchers `ccb` and `ccx` keep
   their own replacement rules. An incomplete comm install ends the enclosing `sotd install` after the version
   switch and pruning, before the daemon restart and install record (docs/defects.md: COMM-REFUSAL-ENDS-INSTALL-MIDWAY).
   Tests in `test/install_tests.jl` cover shared commands for every adapter selection, historical copies and retry,
   the complete install on a collision, partial stages and personal asides, and each current launcher digest.
   `test/publish_tests.jl` covers occupied staging and aside names and marker-name matching.
9. **The shell tools of this folder reach the daemon only through the wire.** The tools are the launchers `ccb` and
   `ccx`, the row CLIs in `spawn/`, `worktree/` and `sot-fe/`, `sot-gh-auth.sh` and the `PATH` wrappers of `claude/bin/`.
   Their route is one of three, and none opens a file the daemon owns, with one named exception: the prompt hook `sot-selection-hook.sh` opens the one file whose path the daemon gives the leg in `SOT_SELECTION_FILE`, which holds exactly the hook's output, and guesses no daemon layout (as the pinned self file is also named to the leg by the daemon). (a) A framed op over an endpoint, through
   comm-lib's `sot_oneshot_request`, which says hello first (the daemon admits nothing before it): `sot-fe`
   (`fe.command.send`, `repl.execute`, `repl.run_file`, `repl.interrupt`, `pty.input`, `pty.screen`,
   `workspace.reauth`, `workspace.restart`, `chat.resolve`, `chat.post`, `chat.post_file`, `chat.delete_post`, `chat.read`, `chat.row_pair`, `chat.channels`, `chat.workspaces`, `version.query`, `workspace.list`), `comm-spawn.sh` and `comm-despawn.sh` (`workspace.create`,
   `workspace.list`, `workspace.destroy`, `version.query`), `comm-bootstrap.sh` and `comm-probe.sh` (`workspace.list`,
   `workspace.create`, `pty.input`, and for probe shutdown `workspace.destroy`), `sot-nav.sh` (`agent.send`, to the hub's relay endpoint). (b) A `sotd` subcommand:
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
   tools' hello is checked by the server (messaging's and the server's contracts), not here. `sot-fe version` prints
   the daemon's product version, wire protocol, declared host and supplied uptime, without the retired lane fields;
   older replies may omit host and uptime. Under the daemon's row it prints one line, `update`, with the state of
   that daemon's update in the daemon's own words (`DaemonVersion.update_line`, wire guarantee 16: the wire's one text
   of a state, sent by the daemon because a shell cannot word one), so it reads as the `UPDATE` cell of `sotd status`
   does (`0.7.4 ready: applies when ...`, `up to date (checked 3 h ago)`, `off: dev build`) and the state is never
   worded a second time in shell. Every control byte of the line (C0 and DEL) is a space before it is printed, since
   the daemon may be another computer's. A daemon built before the field, or a value that is no string, prints no
   line, and the state's fields alone are never worded here. It is the one command that reads the state on a computer
   with no `hosts.toml`, where `sotd status` answers nothing. A `link` row explains that it is the computer's daemon's mail link and
   points to its window for sessions; a window (`fe`) with no session declaration keeps the "declares no sessions"
   line, and other roles keep their existing output (`tests/test-sot-fe-version.sh`). **Shipped tools outside this
   folder's ownership still open daemon-owned paths, and this guarantee does not cover them:** the work-state hooks
   (`comm-status.sh`, `comm-status-heartbeat.sh`, `comm-status-idle.sh`) and the turn auditor, rows' and messaging's, test
   the registry and the inbox and keep turn marks and a signature under the comm folder's `state/` (docs/defects.md: WORK-STATE-HOOKS-READ-FILES); the Windows launcher's stop helper reads `held.json` (docs/defects.md: WIN-LAUNCHER-HELD-JSON). Test: `tests/test-tools-wire-only.sh` (the registry and inbox folders the tools are told of
   do not exist, and a stand-in `sotd` answers the registry and the unread count; worktree status and sync and the
   probe's responder, through one delivered mail and its report, give the daemon's answers and leave the comm folder's
   whole tree as it was), `tests/test-spawn-remote-no-local-row.sh` (a local spawn writes its row through the daemon
   and no inbox file), and `tests/test-sot-fe-endpoint-once.sh` (a `sot-fe` run asks `sotd` for the socket once,
   whichever verb). The other tools are read, not run, by it: their routes are the ones listed.
10. **`ccb` carries no recipe of its own; `ccx` builds Codex's argv itself.** `ccb` finds `sotd` by `$SOTD_BIN` when it is
    executable, else `PATH`, else `~/.local/share/sot/bin/sotd`, else `~/.local/bin/sotd`, and execs `sotd agent-exec
    claude` with the caller's flags in order (guarantee 1), or says `ccb: sotd not found` and exits 1. `ccx` has no
    daemon recipe behind it: it builds the Codex argv itself (`codex [resume <id>]
    --dangerously-bypass-approvals-and-sandbox --dangerously-bypass-hook-trust --no-alt-screen -c 'plugins.sot-row@sot-local.enabled=true' [--no-daemon] <prompt>`), requesting
    inline rendering on fresh and resumed launches. Once per launch, `ccx` runs `codex --no-daemon --help` for a
    fresh conversation or `codex resume --no-daemon --help` for a resolved resume, including a continue. It adds
    `--no-daemon` only on exit 0. The probe has no terminal input or output and waits for the native parser to exit.
    Codex 0.171.0's help describes inline mode as preserving terminal
    scrollback. The stub tests verify only the arguments. The capsule keeps up to 200 rows of scrollback, as it does
    for Claude. A person's check in a real Codex row is still needed to see whether inline history reaches that buffer,
    whether the view survives reattachment and resizing, and how the composer and menus appear in the window
    (docs/defects.md: CODEX-ROW-NO-SCROLLBACK). `ccx` chooses no handle
    (decision 0031): it joins with no name and names, in Codex's first prompt, the handle `comm-context.sh` reads back; a
    join that gives none still starts Codex, with a prompt that names none and one stderr line saying so; it starts a
    fresh conversation unless `--continue` or `--resume` is given (`--fresh` wins over `--continue`).
    `ccx --queue <conversation id> <line>` is the launcher's other mode, a request to a conversation instead of a
    launch: with the binary resolved as for a launch it joins nothing, trusts no folder, writes nothing and starts no
    session. It asks `codex queue --help`, and when that help names both `--thread` and `--message` it execs `codex
    queue --thread <id> --message <line>`, whose output and status are then Codex's own. With no binary, or one whose
    help lacks either option, it exits 69 with one stderr line that starts `ccx: no queue:`, before Codex's queue
    command has run. Its caller is messaging's wake of a Codex row (`rust/backend/src/comm/wake/codex.rs`), which
    never runs an installed `ccx` whose text lacks the mode, since an older one would read the flag as nothing and
    start a session. That test is of the file's text (`--queue)`): a `ccx` of a person's own that wraps the installed
    one lacks it and is refused, and any file at that name whose text holds it anywhere is run with the three
    arguments. Limits: `ccx` runs
    Codex with its approvals and its sandbox bypassed, and is bash: it has no Windows form. Test:
    `tests/test-ccb-agent-exec.sh` (the ladder and the order), `tests/test-ccx-launch.sh` (one quiet parser
    probe, including misleading help text, rejection, a signal and a binary that cannot run; the three `ccx --queue`
    cases), `tests/test-ccx-resume-default.sh` (the resume parser for explicit resume and continue).
11. **The tools run under macOS's bash 3.2 and Git Bash.** They use no bash 4 feature, as messaging's scripts do
    (`comm/CLAUDE.md`). Limit: the proof is the hosted run of `scripts/tests/under-bash.sh` on macOS and on a built bash
    3.2.57 on Linux (a new suite joins by its name); it runs a suite's own body in the newer shell and puts bash 3.2
    first on `PATH` for the scripts the suite starts, so the suites' code is not proved under 3.2; no hosted job runs the
    agents suites under Git Bash, and a local run is under bash 5 and proves nothing of it. PowerShell and cmd cannot
    run them: on Windows each launcher in `~/.local/bin` has a `<name>.cmd` beside it (`_launcher_refusal`,
    `src/launchers.jl`), which PowerShell prefers to the extensionless script in the same folder, and which says to run
    the tool from Git Bash (Claude Code's Bash tool) and exits 1, starting nothing (seen in use: a session's PowerShell
    tool ran `sot-fe version` and waited half an hour with no output, never reaching the daemon; over ssh PowerShell
    refuses such a script at once). Proved on one Windows 11 box: PowerShell picks the `.cmd`, which answers in under
    0.1 s with exit 1. Limit: a comm script named with its `.sh` from PowerShell still goes to Windows' file
    association for `.sh`.
12. **`sot-gh-auth.sh`, the GitHub device-flow sign-in, waits at most a stated time.** Every `curl` call carries
    `CURL_BOUND` (connect 15 s, a call 60 s in all, under 1000 B/s for 60 s), and every `gh` call that reaches GitHub
    runs under comm-lib's `sot_bounded` (`GH_BOUND`, 30 s) and names the expiry on the script's own stderr. The poll's
    expiry counts the sleeps and the time each request took, so a slow GitHub cannot stretch the code's life. Limit:
    `gh auth setup-git`, which writes git's configuration and reaches no network, runs unbounded. Test:
    `tests/test-crlf-gh-auth.sh` ("every curl call carries the stall bound"; "at the script's 30 s bound"; "a slow
    GitHub ends the poll at the code's expiry, by elapsed time").
13. **Where the tools install is fixed by one list.** Every code file directly in each folder `comm/bin-folders.txt`
    lists (`agents`, `agents/sot-fe`, `agents/spawn`, `agents/worktree`) installs flat into the comm folder's `bin/`,
    except that every file `sot-fe` sources by a part line (`src/comm_bin.jl`, whatever their number) is
    embedded in the installed `sot-fe` and is no file of its own; nothing in `claude/` or
    `codex/` installs there. The shared install stage puts `show-result`, `sot-fe` and `sot-gh-auth` in `~/.local/bin`
    for every adapter selection. Each adapter adds its own launcher there. Guarantee 8 governs replacement.
    The wrapper `claude/bin/sot-fe` never replaces the tool `sot-fe/sot-fe`. Test:
    `test/install_tests.jl` ("the agents tools install flat from the four listed folders", and "comm-lib.sh and sot-fe
    install as one file each..." for the embedded text), over the installer's own file list.
14. **No tool leaves a file in the temporary folders, by messaging's rule and with its limits.** On success, on failure,
    killed by a catchable signal and ended twice, no script of this folder leaves a file in `$TMPDIR` or
    `$XDG_RUNTIME_DIR`, by the cleanup-trap rule of `comm/CLAUDE.md` (a shell running `comm-spawn.sh` that stops on
    Ctrl-C says its rollback's outcome). Limits, the rule's own (`comm/lib/comm-lib-base.sh`, cleanup traps): SIGKILL
    runs no trap, so a request killed that way leaves its scratch folder, and a signal that arrives inside the handler
    itself can leave one. Test: `tests/test-temp-files.sh`, whose kill rows stop at marked points, not every one.
15. **A skill body is a template, so it carries no `$` and a digit.** Claude Code substitutes `$<digit>` and
    `$ARGUMENTS` into the text before the model reads it; a skill file of the layer or of the checkout carries no `$0`,
    `$1` or `${1}`. Limit: the rule keeps a skill from being spoiled by a substitution, not from being wrong, and no
    test runs a skill's script or Claude Code. The same suite lints each skill's frontmatter in all four skill trees (its
    `name:` is its folder, its `description:` one line, not a `>` or `|` block, of at most 1024 characters, Codex's own
    limit) and that the install page names every skill and its uninstall line every Codex skill. Check (a lint over the
    files, which a skill with `$1`, a bad frontmatter or a page missing a skill fails): `tests/test-skill-templates.sh`.
16. **A caller that stops reading never ends the spawn or the despawn between steps.** Both run `trap '' PIPE` and
    write every line of their success path with `sot_say` (`comm/lib/comm-lib-base.sh`, a reader that leaves), so a
    reader that leaves (`| head -2`) costs only the lines it did not read: the spawn still waits for ready, sends its
    `--task` and exits 0, and the despawn still removes the handle after its destroy. Under SIGPIPE's default the
    spawn died at its next write, with no EXIT trap, and a row was left ready without the task it was promised. Limit:
    a caller that kills the tool still ends it (SIGKILL runs nothing; HUP, INT and TERM run the spawn's rollback). Test:
    `tests/test-spawn-capsule-workspace.sh` ("a caller that stops reading after the first line never ends the
    spawn"), `tests/test-despawn-resolve.sh` (D6b).

## Owns
- The launch recipe: `agent_argv`, `agent_exec_argv`, `claude_resume_argv`, `claude_recipe`, `resolve_claude`,
  `resolve_ccx`, `agent_env`, `NESTING_ENV_VARS_TO_SCRUB`, `account_spawn_env`, `capsule_supervisor_env`,
  `row_self_file`, `own_sotd_bin`, the awareness env, accounts and their links, the folder-trust record and its
  declaration, the auto-memory flag, and `sotd agent-exec` and `accounts.list`.
- The selection handler (`rust/backend/src/agents/selection.rs`, served `INLINE` as `selection.set`). The server tells the handler whether the connection is a window's
  (`Shared.window`, set once from the hello by `clients::window_name`: role `fe` with a non-empty name); any other
  connection (`cli`, `agent`, `bridge`, an unnamed frontend) is answered `not_a_window` before any row work and changes
  nothing, a file a window set included. The role is the client's own declaration, so this is a protocol check, not a
  security boundary. The row's file
  `<row state folder>/selection.json` holds exactly the rendering of the last selection the daemon accepted for that
  row, or nothing; the one exception is a removal that itself fails, which leaves the old file and answers
  `publish_failed`. The handler resolves the workspace (`unknown_workspace`, an empty id included, writes nothing),
  resolves `node_id` with files' read resolver, the preview's (`bad_node_id` for an empty id, no `files:` prefix, `..` or
  an absolute path, or a project root missing when the row was registered; a root deleted later is `not_file_text`), and so
  follows a link out of the root as the preview does,
  never further than the preview reads; it reads at most the preview's 2 MiB (`PREVIEW_BYTE_CAP`) of a regular file (a
  directory, a pipe or device, a missing or unreadable file is `not_file_text`), then `check_selection` and
  `render_selection`. An accepted selection is published by platform's durable publish (a whole file or none) and
  answers `shared` when the marker `<row state folder>/selection-hook` exists (the row's current agent was started with
  the hook) and `held` when it does not; `claude` and `codex` rows (`TAKES_SELECTION`) take text. Any other
  kind answers `no_agent` and loses its file; a clear removes the file and answers `shared` for any row; every refusal
  (`not_file_text`, `bad_node_id`, `no_agent`) and a failed publish (`publish_failed`) removes it first. A removal that fails
  answers `publish_failed` in place of the refusal's own code, and the old file stays. A request the wire refuses at
  decode (a start not before the end, a `len` outside 1 to 2 MiB, an over-long id) is answered `handler_error` and
  changes nothing, so the previous file stays. The row's state folder
  is the capsule's and exists once the row has started; for a row that never started an accepted selection answers
  `publish_failed` and a clear answers `shared`; no marker exists there either. The relative path in the rendering is
  files' `path_to_node_id` less its prefix, so on Unix a file named `a\b.txt` is shown as `a/b.txt`. A file swapped for a
  named pipe between the type check and the open blocks the connection loop, the stall the server contract states.
  For a Claude leg the marker is written by `rows::spawn::detach::spawn_detached_supervisor` (both callers: the first start and the
  watchdog's restart) through `selection::write_marker`, after a launch that started a supervisor (on Unix a contended launch
  started nothing and leaves the live supervisor's marker; on Windows the launch always reports a start, and a launch whose
  supervisor then exits 70 on the held fence has written the marker from its own argv over the live leg's, which is wrong
  only when the two argvs differ): it exists exactly when the row's kind is `claude`, the leg's
  environment carries `SOT_SELECTION_FILE` and its argv's `--settings` carries the selection hook on UserPromptSubmit
  (`layer::carries_selection_hook`); it is removed otherwise. `selection::leg_hooked` is the one reading of the marker's meaning, that the row's current leg was launched with the row's hooks. `publish` reads it only to choose `shared` or `held` for an accepted selection (a kind that takes none answers `no_agent` whatever the marker says); the second reader is messaging's `wake::codex::not_woken_by_mail`, which marks a Ready Codex row whose leg has neither the marker nor a binding.
  A supervisor started before the release has no marker. The marker describes the last launch: between a leg's exit and
  its restart, or after a downgrade and an upgrade that adopts the older supervisor, it can describe another leg; and a
  failed write or removal (logged as not brought up to date) leaves the earlier launch's marker. A reset Claude leg in the same supervisor runs with that supervisor's
  argv and environment, so the marker stays true for it; it survives a daemon restart. The Unix Codex producer removes the marker before running any installed
  `ccx --capsule` on every leg. If the producer cannot clear the old marker, the row's agent does not start and the
  watchdog stops restarting the supervisor when its budget runs out. The producer passes the marker path as
  `--selection-marker`. At native exec, ccx writes it when the row selection path is set, the outgoing argv enables the
  row plugin and includes `--no-daemon`, the installed script is executable and the sole cached version declares
  the matching hook file with exactly the selection, binding and context recovery commands at their expected events. If ccx cannot
  write the marker, Codex still starts and text answers `held`. A failed exec removes a marker ccx wrote. The daemon leaves this write to ccx.
  **Run UUID.** The producer clears the previous leg's marker. Adoption after a downgrade can retain one.
  **Hook receipt.** The marker records the launch checks. It cannot confirm a callback or a later settings change.
  **Status probe.** Until the next leg starts, a selection can read the last leg's marker after that leg exits.
  **Shared background server.** With `--no-daemon`, Codex runs the session and invokes its prompt hook in the row's
  own process environment. On older Codex versions without the flag, the session starts and text answers `held`.
  The daemon still writes accepted text to the row's selection file in this case.
  Isolation across simultaneous fresh, resumed and ordinary sessions still needs a window check.
  The absolute path in the rendering is the operating system's spelling (backslashes on Windows), which the
  file's JSON escapes. The published text is the file's text at the read; a file that changes
  later leaves that snapshot until the window clears it. Withdrawal: one process-wide map behind one lock holds per
  row the roster serial of the connection that set the file and the file's path; the serial comes from one counter for
  the process (`clients::NEXT_SERIAL`), so it is never reused. The row is resolved and the project file read and
  rendered before the lock is taken: `apply` takes the rendered text, so a render under the lock needs a visible edit;
  no test pins that scope. The lock is never held across an `.await`, so two connections setting one row are serialized and the newest accepted selection wins and owns the file. A clear
  or refusal from any connection removes the file whoever set it. An accepted selection records its setter; a clear, a
  refusal after the row resolves, `no_agent` and a failed publish drop the row's entry once the file is gone; when a
  removal fails the file stays and so does its entry, so the setter's end or a later clear retries it; `not_a_window`,
  `unknown_workspace` and a missing state root change nothing. `selection.set` stays `INLINE` partly for this: it
  runs on its connection's own loop, so no job can record an entry after that connection's `forget`. `ClientGuard::drop` calls `selection::forget(serial)`,
  which removes every file that serial set and none another set (a redialed window's old connection cannot erase what
  its new one sent). `rows::run::resume::resume_all` calls `selection::clear_unowned` for every registered row first,
  which removes the row's file only when it exists and the map holds no entry for it (a row with no file costs one
  `stat`), so a file an earlier process left goes and a
  share made before the walk reaches its row stays. Once the walk is done every file names a selection a live
  connection of this process set. A poisoned lock is taken as it is (each change to the map is one insert or remove).
  A drawing (`SelectionSetReq.drawing`) is the same slot: after the window check, the state root and the row, the handler
  checks `takes_drawing` (Claude on all
  supported hosts and Codex on Unix; other kinds answer `no_agent`, and their kernel is asked nothing), has files' `page_png` ask the row's kernel for the page when the base names one (a PDF's page; before the lock, so a wait for one row's kernel, at most its 10 s bound, holds no other row's drawing; a page not drawn is `not_an_image`, and so, with the kernel never asked, are page 0 and a node that is no regular file whose path ends in `.pdf`: files' contract), takes the process-wide compose lock (a `tokio::sync::Mutex`) and, under it, has files' `compose` write the picture on the blocking pool (off the connection's loop),
  renders it with `render_drawing` and publishes through the same `apply` under the setter lock. The compose lock spans
  the picture's write and the file's publish, so two drawings from two connections go one at a time and, after each
  drawing's publish, the file names the picture on disk and the digest it holds (the digest is `selection_digest` of
  the drawing's JSON as the wire serializes it). A refusal after the picture's write (`bad_node_id` on the figure's
  second resolve, `publish_failed`) removes the file and leaves the picture on disk, named by nothing; a refusal of the
  composite leaves the old picture the same way; a clear removes the file and never the picture; a setter's end and
  the walk withdraw a drawing as they withdraw text; a text selection and a drawing replace each other, the newest
  owning the file. Limits: the picture is one file per row, overwritten, so between a later drawing's picture write and
  its publish, and between a prompt and the model's read, the picture can hold a newer drawing than the digest the
  agent was given (also when a publish and its removal both fail); the next prompt carries the new digest. A drawing
  waits for the compose lock behind every other connection's composite, for any row, and its connection's loop waits
  with it; a text selection never waits on it. A drawing on a page is composed over the page the kernel draws when the
  share arrives (`selection::tests::a_page_drawing_is_composed_over_the_kernels_page`, Unix, a stand-in kernel: the
  published file names the file and the page, a page the kernel did not draw withdraws it, and a row that takes no
  drawing asks no kernel); no test runs it with the real PDF plugin, a real window or a real agent.
  Limits: a removal that fails at a connection's end or in the walk has nobody to answer: it is logged once, the
  entry is dropped, and the next walk removes the file; the startup Cleanup runs no walk, so a row it could not end
  keeps an earlier process's file until a later resume; on Windows a publish or removal that meets an open read can fail, which is
  `publish_failed` and the old file may remain. A real Codex window still needs to confirm the notice, source lines, full span
  and that the agent opens the named PNG and sees the marks (docs/defects.md: CODEX-SELECTION-WINDOW). The window's
  clear after an error reply (A7) remains unbuilt. Tests:
  `selection::tests::the_row_file_holds_the_last_shared_selection_or_nothing`, one
  row per path (a failed removal and a link to a device among them), `the_replies_are_the_wire_frames`
  (a decode refusal leaves a published file), `the_reply_says_who_will_hear` (`no_agent`, `shared` and `held`), `the_marker_follows_the_leg_that_wrote_it` (the marker: written for a Claude leg with the variable and the hook, removed for a Claude leg that lacks either, left to ccx for Codex), `tests/selection.rs::a_window_selection_reaches_the_rows_prompt_hook_and_its_end_withdraws_it` (the whole path, a real daemon and a stub `claude`: the hook prints the selection, then nothing after a clear, then nothing after the window's end), `a_setters_end_or_the_start_up_walk_withdraws_only_stale_selections` (the setter map, a real guard's end, `clear_unowned`), `a_drawing_is_the_rows_selection_until_it_is_withdrawn` (publish, clear, refusals, kinds, replacement and both withdrawals for a drawing), `two_drawings_at_once_leave_the_file_naming_the_picture_it_holds`, `tests/selection.rs::a_window_drawing_reaches_the_rows_prompt_hook_and_its_end_withdraws_it` (the whole path: the hook names a picture that decodes, the window's end empties it),
  `rows::run::resume::selection_tests::the_walk_clears_each_registered_rows_unowned_selection` (the walk's call); `server::context::tests::only_a_named_frontends_connection_may_share_a_selection` (real connections of each
  role: only a named `fe` is not `not_a_window`).
  `tests/selection.rs::codex_selection_follows_the_launcher_across_resets` covers the older and current launchers,
  contention, text and drawing replacement, a marked PNG at the path printed by the hook, drawing withdrawal,
  a held drawing on the older launcher and the exhausted restarts after a marker cannot be cleared.
  In `tests/test-ccx-launch.sh`, the `selection marker at native exec` cases exercise each stamp condition, an
  unwritable marker and a failed exec. `tests/test-ccx-resume-default.sh` checks the marker for both explicit resume
  and continue, with and without flag support. In `test/codex_tests.jl`, `installed ccx marks the installer's row
  plugin at native launch` runs the installed launcher against the published cache.
- The selection check and its rendering (`rust/backend/src/agents/selection_check.rs`, pure, no I/O, called by the handler).
  `check_selection` accepts exactly the file's own text at the
  window's range: it refuses (`not_file_text`) a start not before the end (the wire's decode refuses it too; the function
  does not depend on the decode), bytes that are not UTF-8 before the range's end, a CR not before an LF or
  a U+2028, U+2029 or U+0085 before the end, a position outside its line or inside a character, and a `len` or `digest`
  (`sot_protocol::selection_digest`) that differs from the file's text; the wire refuses a `len` outside 1 to 2 MiB and a start
  not before the end, but checks neither `len` against the text nor any position, so this is the only place a position near
  `u32::MAX` or a wrong `len` is refused. The bytes may be a prefix ending mid-character after
  the range. `render_selection` builds the hook's one JSON object (`hookSpecificOutput` and `systemMessage`, nothing
  else) with 1-based inclusive lines. Every cap counts UTF-16 code units, Claude Code's own count and never below a
  character count: the range's text is cut at 2000, the path in the context at 4096, the message at 200 (a path cut
  from the left with `...`), and a compile-time assertion keeps the context under 9,000. The cut's suffix names the
  range's whole size in Unicode scalar values. A control character, U+2028 or U+2029 in a path is shown as `?`.
  `render_drawing` builds the same two-key object for a drawing: the context names the figure's path (none for the
  canvas; for a page, `page <n> of` the file's path), the picture's path, the drawing's digest as 16 hex digits and asks the agent to read the picture; the message
  is `Drawing: <path in the project or sketch> (<N> strokes)`, with ` page <n>` after the path for a page; the same caps hold, by a compile-time assertion at two
  4,096-unit paths and a ten-digit page. Tests: `a_drawing_renders_as_the_hook_output_within_the_caps`, `a_page_drawing_names_the_file_and_the_page`,
  `selection_check::tests::only_the_files_own_text_at_the_range_is_shared`,
  `selection_check::tests::a_whole_lines_range_is_the_files_own_lines`,
  `the_rendering_is_the_hook_output_both_agents_accept_within_the_caps`.
- The layer: `<comm home>/claude-layer/` (written by the installer from `agents/claude/` and `comm/adapters/claude/`,
  read by the recipe) and the table of hooks in `layer.rs`.
- The non-messaging Claude skills (the four onboarding ones global, the rest the layer's; `julia-repl`, `project-log`, `reauth`, `show-result`, `sitrep`, `sot-chat-pair`, `sot-chat-post`, `sot-gh-auth`,
  `sot-install`, `sot-setup`, `sot-status`, `sot-statusline-setup`, `worktree`) and the Claude launcher `ccb` live in
  `claude/`. The shared install stage takes its three PATH wrappers from `claude/bin/`. The Codex launcher `ccx` lives in `codex/`.
- The row CLIs `spawn/`, the /worktree scripts `worktree/`, the frontend CLI and nav broadcast `sot-fe/`,
  `sot-gh-auth.sh`, and the suites `tests/`.
- `sot-fe docs <workspace> <absolute-page>` carries an explicit entry reply scope through `fe.command docs`:
  ordinary by default, `--direct` for the named row, or `--direct-all` for all rows (the list pages). The two flags are
  exclusive and accepted only for docs. The daemon resolves the row to its canonical id and refuses a row-scoped
  page outside its project folder; the CLI never infers authority from a document. Navigation to another HTML
  document is ordinary, and the page reads `states.reply_scope` before enabling scoped controls. A scoped page's reply
  is typed directly; an unscoped page's is refused (pages' contract). `tests/test-sot-fe-paths.sh` records the scope in
  the actual request; `tests/test-sot-fe-goto.sh` covers invalid flag combinations.
- `sot-fe chat post [--thread <ts>] (<text> | --stdin)` (`sot-fe/sot-fe-chat.sh`) is a row's reply in its channel. It
  posts only for the row it runs in (`$SOT_WORKSPACE_ID`, none refused before an endpoint is found) under the handle
  `comm-context.sh` gives (none refused the same way), asks `chat.resolve` for the pair, sends one `chat.post` of kind
  `row` and never a second (it does not use `send_fe_command`'s retry: chat guarantee 12), and waits 50 s for the post: past the daemon's own bounds with a margin, the member list's 10 s for a name to link and the post's 30 s (chat guarantees 35 and 14), so a made post or a refusal is never read as "may exist" (`case_each_wait_is_past_the_daemons_own_bounds` reads sot-chat's constants and the script's waits).
  The text, handle and thread reach `jq` on stdin. Exit 0 with `posted <ts>`, then the report in wire's one form (`told_words`, which jq copies): `; mentioned <n>:
  <entries>` for the members the post mentions and `; not linked: "@<name>" (<reason>[, <n>: <candidates>])` for each
  `@` name the daemon left as text, entries as the daemon made them (`"display" ("real") @handle`), a count before each
  list (the copy is pinned to the wire's: `case_sot_fe_prints_the_wire_report_for_every_case` reads the pairs in
  `rust/protocol/src/ops/told_words_cases.tsv`, which the protocol test asserts too); 1 for usage; 2, nothing posted for certain, with
  `refused <code> <words>` or `limited, wait <n> s`; 3, the post may exist, with `unknown: ...` (also a `handler_error`
  reply) or `no answer: ...` (also a hello the daemon refused, which cannot be told apart), each ending "do not send it
  again; to see whether it went out, run sot-fe chat read" and, for a post into a thread, " --thread <ts>" of that thread (the
  words of `_chat_may_exist`; `sot-fe chat read` says what the read prints). A caller tests the exit code, and never posts again on 3. The daemon refuses a row post under any name but the row's bound handle (chat contract, D13).
  `tests/test-sot-fe-chat.sh` runs it against a stub daemon: outside a row and with no handle refused before an
  endpoint, one resolve then one post with the handle and thread, each outcome's words and code, no answer as one
  request, and `--stdin` text with newlines, quotes and a leading `/` whole. The skills tell an agent to reply with it
  (messaging's `sot-comm`); a Codex row is woken for channel mail as for any mail, through Codex's own queue and never by typing (messaging's contract, Waking). A person's plain ask to post in the row's Slack
  channel finds the verb through `sot-chat-post` (`claude/sot-chat-post/`, and `comm/adapters/codex/skills/sot-chat-post/`
  for a Codex row, the two files byte-identical, pinned by `tests/test-skill-copies.sh`, and every skill's frontmatter linted by `tests/test-skill-templates.sh`; the Codex copy sits in messaging's adapters
  folder but is agents'), whose description carries the asks' words; the installer's directory walk installs it, so no
  installer list names it (the manual's install page lists the installed skills and the uninstall line by hand). It says the verb posts only to the row's own channel, publicly, and refuses another channel or a direct message; what the
  exit codes mean; that a row with no channel is told what the command printed; and
  that an `@name` in a post mentions the one member it names, whom it mentioned and the names left as text are printed
  after `posted` and are read back to the person, and a row's `@here` or `@channel` never notifies (chat guarantee 35). Unproven: that a session, asked in plain words, loads it.
- `sot-fe chat post [--thread <ts>] --file <path> [<comment> | --stdin]` shares a file of the row's folder (a figure, or
  any regular file of at most 16 MiB) under the same rules: the row and handle refused as above, then the path (a
  relative one joined to `$PWD`) made relative to `$SOT_WORKSPACE_ROOT` by comm-lib's `sot_relative_to`, the one owner
  of that rule (one spelling of both, so Git Bash's `/c/...` meets a `C:\...` root; a link above the folder resolved,
  one inside it never), and refused (exit 2, before an endpoint) when it is outside the folder, the folder itself, or has
  a `..` part; the rest is the node id `files:<rel>`. It sends one `chat.post_file` of kind `row` with that id, the
  thread and the comment, waits 145 s (past the member list's 10 s, the upload URL's 15 s, the upload's 60 s, the share's 30 s and the read of the share's time, 15 s, with a margin; pinned by the same case), and prints `posted <ts>` or, when neither Slack's share answer nor the daemon's one `files.info` read named the message's time, `posted file <id>` (the mention report follows on that line) and a second line, `its message's time is not known yet: run sot-fe chat read[ --thread <ts>], whose line for this post shows files=<id>` (`case_a_file_post_with_no_time_says_how_to_find_it`), with the
  same exit codes. The daemon reads the file itself and refuses anything not a readable regular file of the folder, and
  the chat's configuration files (chat contract, D13). Unmeasured until a person's first use: that `--thread` shares
  into the thread and that the share reads back as the row's (the figures plan's LP2 and LP4). The suite runs on Linux
  and macOS bash 3.2; the Windows spelling rests on `sot_relative_to`'s own tests, and no Git Bash run of this verb has
  been made. `tests/test-sot-fe-chat.sh`'s file cases: outside a row, the folder or with no handle refused before an
  endpoint, one resolve then one `chat.post_file` with the node id, thread and comment (or neither), a path spelled
  through a link above the folder, each outcome's words and code, and no answer as one request.
- `sot-fe chat delete <ts> [--thread <ts>]` (`sot-fe/sot-fe-chat.sh`, `send_chat_delete`) deletes one post of the row's own channel,
  the message with that Slack `ts` and every file this project's Slack app posted with it, files first (chat guarantee 36),
  which this project's Slack app posted (this session's own posts, and those of earlier sessions of the project; nothing in
  another project's channel). A file a person uploaded stays, and so does a file the app can see shared anywhere else in Slack (a share the app cannot
  see, a direct message or a channel it is not in, is not known to it, so such a file is deleted with the post; check C4 of
  CHAT-DELETE-UNMEASURED is owed), and a line names each with why. It runs for the row it runs in and under its handle as a post does (none of either refused before an endpoint is
  found), asks `chat.resolve` for the pair, sends one `chat.delete_post` for the `ts` (and the thread of a reply, `--thread`)
  with the row, the handle and the resolved pair and never a second, and waits 135 s (past the whole delete's 120 s,
  `DELETE_POST_DEADLINE`, with a margin; pinned by `case_the_delete_wait_is_past_the_daemons_own_bounds`). The verb names
  no channel and no file, so no session chooses either, and `sot-fe` refuses anything but one `ts` of Slack's form (`<seconds>.<six
  digits>`), a `--thread` that is not one, a `--file` and a `--stdin`, with exit 1 before anything is sent. Exit 0 with
  `deleted <ts>` or `gone <ts>: ...` (without `--thread`: no message with that time at the top of the channel, a reply was
  not looked for; with it: the thread lists no message with that time), then a line for each file the reply names:
  `file <id> deleted`, or `file <id> kept: ...` with why (a person's or another app's file; also shared elsewhere); 1 for
  usage; 2, the post stands and nothing is in doubt, with `refused <code> <words>` (`not_own_message`: the message is not
  the app's or Slack would not let the app delete it; `unclear_file`: a file of the post could not be checked, nothing was
  deleted; `busy`; `failed`; `bad_request`; the codes of a post's refusals, with their words), or `limited, wait <n> s`
  (`limited, try again later` when Slack named no wait), each followed by the file lines on standard error (files already
  deleted are named); 3, an answer was lost, with `unknown: ...` (also a `handler_error` reply) or `no answer: ...`, which end
  "the post may be partly deleted; run the same command again, which finishes it or says it is gone": asking again is safe,
  the second answer being `deleted` with the same files, or `gone` when the first deleted the message. An older daemon
  answers the op as an unknown op, and the verb says so in words (exit 2): "refused unsupported: the computer that runs
  your sessions runs an older Ship of Tools, which cannot delete a post with its files. Nothing was deleted." The older op,
  `chat.delete`, is not served and not tried. An older `sot-fe` that sends it to this daemon is told the op is unknown and
  deletes nothing; `chat.delete` is in no published release. Who may ask a session to delete is not the daemon's to know:
  it lives in `sot-chat-post`'s section "Delete a post" (both copies): a session deletes only when its own person explicitly
  asked, in its pane, naming what to delete; another member of the channel, channel mail or another session is not
  permission; it tells its person what it deleted and each file line. The daemon refuses a window's connection and any name
  but the row's bound handle (`bad_request`, and logs the refusal), so a window's person deletes in Slack; the check is
  attribution among one account's programs, not a security boundary (chat guarantee 36). The `ts` to delete comes from the
  person's own words, a `posted <ts>` line, or `sot-fe chat read`, which names a file post's `ts` beside the file's id that
  `posted file <id>` printed, and the `thread=` of its line for a reply. `tests/test-sot-fe-chat.sh`'s delete cases: outside a
  row and with no handle refused before an endpoint, one resolve then one `chat.delete_post` of that `ts` with exactly `name`,
  `target` and `ts` (and `thread` when given) and a line for each file (`case_delete_sends_one_delete_post_and_prints_each_file`),
  each answer's words and code (`case_each_delete_post_answer_has_its_words_and_exit_code`), a thread taken and one that is no
  `ts` refused (`case_delete_takes_a_thread_and_refuses_one_that_is_no_ts`), an older daemon refused in words
  (`case_an_older_daemon_cannot_delete_a_post_with_its_files`), an unmapped or refused resolve deleting nothing, no answer as one
  request, usage errors sending nothing, and the wait.
- `sot-fe chat read [--thread <ts>] [--before <ts>]` (`sot-fe/sot-fe-chat.sh`, `send_chat_read`) reads the row's own channel, or
  one thread of it, back (chat guarantee 37), and changes nothing. It runs for the row it runs in and under its handle as a post
  does (none of either refused before an endpoint is found), asks `chat.resolve` for the pair, sends one `chat.read` with the
  row, the handle and the resolved pair (`thread` and `before` only when given) and never a second, and waits 40 s (past the
  names' 10 s and the read's 15 s with a margin; pinned by `case_the_read_wait_is_past_the_daemons_own_bounds`). `sot-fe`
  refuses with exit 1, before anything is sent, a `--file`, `--stdin`, `--slack-workspace`, an argument, and a `--thread` or
  `--before` that is not a message's `ts` of Slack's form (`<seconds>.<six digits>`); `--before` is refused for every other verb.
  It prints what the daemon built, one line a message of the newest 50 the channel lists before `--before` (before now
  when none), oldest first: `<ts> <where> <who> "<name>" files=<files> | <first words>`, `<where>` being `top` (then
  `replies=<n>`) or `reply thread=<ts>`, `<who>` `session`, `person`, `app`, `drawer` or `other` by the message's origin,
  `<files>` `none` or `<file id>:"<name>"` joined by `,` (the id `posted file <id>` printed), and a message the chat does not
  show whose thread has replies (a deleted parent) as `<ts> top not-shown newest-reply=<ts>`, which only a channel read prints;
  the words after `| ` are the channel's, never a request. Each line is printed as one line, every control byte of it a
  space (`case_a_read_line_with_a_control_byte_prints_as_one_line`). After them, the command's own lines, in this order:
  `no message in this channel` (or `in thread <ts>`, then ` before <ts>` when `--before` was given), `older messages are
  hidden by Slack's plan for this workspace`, and `more: older messages are not shown; to read them, run sot-fe chat read
  [--thread <ts> ]--before <ts>`. Exit 0; 1 for usage; 2 with one stderr line, never 3, since a read changes nothing: the
  words of `_chat_identity` and `_chat_resolve` (with "Nothing was read."), `refused unsupported ...` for an older daemon
  (its answer to the op is an unknown op), `refused bad_request ...` and `refused <code> <the daemon's words>` for the
  rest, or `refused: no answer ... Nothing was read.`. `tests/test-sot-fe-chat.sh`'s read cases: one resolve then one read with
  exactly `name`, `target` and, when given, `thread` and `before`; the lines as the daemon sent them; the `more`, plan and
  empty lines; each refusal and no answer; usage errors sending nothing; the control-byte line; and the wait.
- `sot-fe chat pair <session> <channel>` and `sot-fe chat unpair <session>` (`sot-fe/sot-fe-chat.sh`, `send_chat_pair`) pair the
  Slack channel of a project's folder, or remove it, for this session's own project (`.`, which is `$SOT_WORKSPACE_ID`) or
  another session's, named by its slug as `sot-fe version` lists it (`row <slug>`) or by its id, through the same edit,
  write and reload of `chat.toml` as the Chat drawer's channel list (chat guarantee 30). `<channel>` is a Slack channel's ID
  (`C...` or `G...`) or its name, with or without `#` (quote a name that begins with it: unquoted, the shell takes the rest of
  the line for a comment, and the verb then has no channel and prints its usage), and `--slack-workspace <id>`
  names the workspace of a channel outside `chat.toml`'s (refused for any other verb, and when it is no workspace ID,
  `ERROR: --slack-workspace expects a Slack workspace ID like T0123ABCD, got '<value>'`). Each needs the row and the handle as a
  post does (none of either refused before an endpoint is found), sends no `chat.resolve` and ONE `chat.row_pair` with `by`
  (this session's row and handle), the session as typed and the channel (`null` to unpair), never a second, and waits 150 s
  (`--timeout` changes it): past a reload under the setup lock in the ordinary case and not the worst case (chat guarantee
  21's limit), so a no-answer is exit 3, and a pairing is idempotent, so the same command again is safe. `sot-fe` refuses
  `--thread`, `--file` and `--stdin` (`ERROR: --thread, --file and --stdin are only for chat post`) and a wrong count of
  arguments with exit 1, before anything is sent. Exit 0 with `paired <folder> with <chan> in <at>` (`; new posts there reach
  this project's session as mail` when the file delivers, `that project's` for a session other than this one, which is `.` or
  its own id: `case_pairing_this_sessions_own_project_says_this_projects_session`), `unpaired <folder> from <chan> in <at>`
  (`; new posts there no longer reach this project's session as mail` or `that project's` by the same rule, when the removed
  pairing delivered, `case_unpairing_a_delivering_channel_says_its_posts_stop_reaching_the_session`; `<chan>` is `#<name>
  (<id>)` when the daemon's cache or the channel's facts gave the name, else the id; `<at>` is `<tag> (<workspace id>)` when
  the workspace's short name is known, else `Slack workspace <id>`), `unchanged: <folder> already has <chan> in <at>` or
  `unchanged: <folder> has no channel`; 2, nothing changed for certain, with `refused <code> <words>` (`stale_row` (also a row
  whose folder cannot be reached), `unmapped`, `unavailable`, `not_saved`, `bad_request`, `already_paired` (the project has
  another channel: move it by unpairing, then pairing), `channel_taken` (a channel any other project has, a gone folder's
  included: a person's pick in the window), `several_channels` (a name more than one channel has: the ids are listed, and the
  session names one by its ID), `no_such_channel` and `not_in_channel`, each in the verb's own words, which differ
  by verb where the code means more for one, `<session>` as typed and `this session's project` for `.`; any other code in the
  daemon's text) or `refused unsupported ...` for an older daemon's unknown-op answer (it lets only a person pair, or pair or
  unpair, in the Chat drawer's channel list); 3, it may have been made, with `no answer: ...` or `unknown: ...` (a
  `handler_error` reply, or an outcome added later), which end "running the same command again is safe and says what is there
  now". For `no_such_channel` and `not_in_channel` the verb reads `chat.workspaces` once (`--timeout`, else 10 s) for the
  workspace's name, its Slack app's name to invite, and, on a computer that connects several workspaces, the list with
  their IDs (`; this computer connects these Slack workspaces: <id> (<name>), ...: name another with --slack-workspace <id>`);
  with no answer the words say "that Slack workspace" and "your Slack app's name". A pair that names a workspace the hub does not
  connect (`unavailable` with `--slack-workspace`) reads it too, and says that workspace is not connected and lists the ones that are. A
  refusal whose body says `joined` (the Slack app is in the channel, and a window's pairing or a hand edit landed first) adds
  that the app is now in the channel and nothing was paired; an `already_paired` with no channel in its body (never seen from
  the daemon, which names the mapping the file holds) drops the channel from its words. Who may ask a session to pair or unpair is the
  person's, not the daemon's: it lives in the skill `sot-chat-pair` (`claude/sot-chat-pair/` and, byte-identical,
  `comm/adapters/codex/skills/sot-chat-pair/`, pinned as `sot-chat-post`'s are): only the projects and channels the person
  named, a move only when asked (unpair, then pair), each change reported, never in a loop; `sot-chat-post`'s "A row with no
  channel" says an unmapped project can be paired with it. The daemon checks `by` as a row's post is checked (attribution
  among one account's programs, not a security boundary) and logs each changed pairing and each refusal with who asked. The
  150 s default wait has no case that pins it past the daemon's bounds, as the post's and the delete's have (the harness always
  passes `--timeout`). `tests/test-sot-fe-chat.sh`'s unpair and pair cases:
  `case_unpair_sends_one_row_pair_with_the_handle_and_prints_what_it_removed`, `case_each_unpair_answer_has_its_words_and_exit_code`,
  `case_an_older_daemon_is_refused_unsupported`, `case_no_answer_to_a_pairing_is_one_request_and_exit_3`,
  `case_pair_sends_one_row_pair_with_the_channel_as_given_and_prints_the_pairing`, `case_each_pair_refusal_has_its_words_and_exit_code`,
  `case_a_channel_not_found_on_a_hub_of_several_lists_the_workspaces_and_the_app` and `case_pair_verbs_refuse_the_post_flags`. Lines the
  chat verbs share and that name project words: the refusals of `_chat_identity` (the unset `SOT_WORKSPACE_ID`, "comm handle") and the line
  `_need_endpoint` prints when no daemon is found, which every `sot-fe` verb prints.
- `sot-fe chat workspaces` and `sot-fe chat channels [<text>]` (`sot-fe/sot-fe-chat.sh`, `send_chat_workspaces` and
  `send_chat_channels`) are the reads a pairing's words can be compared with (the maintainer asked on 2026-11-25, after a first pairing
  in use was refused for a channel name the hub's Slack app could not see, that a session can list the available workspaces and channels).
  They sit beside `chat pair` as its two nouns: `channels` is what `pair` names, `workspaces` is what `--slack-workspace` names. Each needs the row
  and the handle (refused before an endpoint is found), sends ONE `chat.workspaces`, or ONE `chat.channels` with `by` (this session's row and
  handle) and the workspace when `--slack-workspace <id>` names one (chat guarantee 30 says which workspace the daemon lists
  otherwise), and changes nothing in Slack or `chat.toml`, so every failure is exit 2 with `refused <code> <words>` and no verb here has an exit 3
  (no answer is `refused: no answer ... Nothing was changed.`). `workspaces` prints `<id> default|other running|stopped <name>`, the name last
  (`(unnamed)` when Slack gave none). `channels` prints `workspace <id> <name>` (the workspace a `chat pair` with none named goes to, `chat.toml`'s default, or the one `--slack-workspace` names, whichever project asks; one more `chat.workspaces` read gives the name, else the id alone),
  then one line for each channel in the same five fields, `<id> #<name> public|private app-in|app-out free|taken` (`taken`: any mapping of `chat.toml` holds the channel, a gone folder's and another workspace's included, so another project's pairing of it is refused `channel_taken`; the asking project's own is `taken` too, and a pairing of it is answered `unchanged`), then, when the text
  matches none of them, `no channel name contains "<text>"` (`your Slack app can see no channel here` with no text), and, when Slack's list was cut
  at its bound, a last line that says a channel not shown can still be paired by its ID. `<text>` keeps the names that contain it, a leading `#`
  dropped and ASCII capitals ignored as `Api`'s `named` compares a name for a pairing (quote one that begins with `#`); a second argument is exit 1.
  `--slack-workspace` is the pair and channels verbs' alone. The wait is 90 s (five pages of Slack's list, each within a read's 15 s;
  `--timeout` changes it) and a workspaces read's 10 s. `bad_request` is what a daemon older than the listing answers any session's
  `chat.channels` with, and also what a handle that is not the row's gets, so its words say both; `unavailable` says the chat does not run there or is stopped, as the pair verb's does, and with a named workspace the
  computer does not connect says that, as the pair verb's does. The `no_such_channel` refusal of `chat pair` ends its words with
  `; to see the channels your Slack app can see, run sot-fe chat channels` (with `--slack-workspace <id>` after it when the pair named one).
  The skill `sot-chat-pair` (its Codex copy is byte-identical) tells a session to list first when a person names a channel by name or a pairing was
  refused as `no_such_channel` or `several_channels`, to pair by the ID the listing printed, always with `--slack-workspace <the listed workspace's ID>`, to tell the person the nearest names when theirs is not listed, and never to pair another
  channel than the one the person named without the person's yes. `tests/test-sot-fe-chat.sh`'s cases:
  `case_workspaces_is_one_read_and_prints_one_line_each`,
  `case_channels_sends_one_request_with_the_row_and_handle_and_prints_one_line_each`, `case_channels_text_narrows_by_name_as_a_pairing_compares_names`,
  `case_channels_says_when_the_list_was_cut_and_when_it_is_empty_or_the_name_is_unknown`, `case_each_channels_refusal_has_its_words_and_exit_code`
  and `case_the_listing_verbs_are_refused_outside_a_session_and_on_bad_usage_sending_nothing`.
- `show-result` opens no `.html` or `.htm` page (the maintainer, 2026-10-31: pages went to the browser, not to the
  Queue): for a page under the calling row's project folder (`SOT_WORKSPACE_ROOT`) it prints the line that offers it,
  `Page: <absolute path>` on the session's closing marker, and exits 0; a page elsewhere, which the Queue cannot open,
  is refused with exit 2. It runs no `sot-fe` for a page; a session opens one only when its person asks, with
  `sot-fe docs`. Everything else badges through `sot-fe preview` as before. `tests/test-show-result.sh`.
- `sot-fe restart <session>` (`sot-fe/sot-fe-row.sh`, `send_workspace_restart`): the words and exit codes of rows' `workspace.restart`,
  which starts a marked Ready Codex session's agent again under this computer's Ship of Tools, in a new conversation (the
  daemon's rules are rows'). `<session>` is as typed, a slug as `sot-fe version` lists it after `row`, or an id. The verb
  refuses `.` and the caller's own `$SOT_WORKSPACE_ID` before an endpoint is found (exit 2, nothing sent), takes exactly
  one argument (else the usage, exit 1), and sends ONE request, never a second, waiting 60 s (`--timeout`). The request
  carries `$SOT_WORKSPACE_ID` as `by` when it is set, so the daemon refuses the caller's own row by any name (`own_session`;
  the id is the caller's claim, as a `pty.input` origin is). A refusal of the daemon prints `refused <code>: <the daemon's
  words> Nothing was restarted.` and exits 2; an older daemon's unknown-op answer is `refused unsupported: ...` (exit 2).
  `restarted <slug>: ...` exits 0; its line says the agent was started again and not that mail now wakes it: that needs this
  computer's install to start Codex with its hooks, so the line tells the person to look at the mark again, to run the
  installer and check that Codex accepts `--no-daemon` if it is still there, and not to run this again for that. A stop that
  did not finish (`unknown: ...`), an agent that was stopped and not started (`not started: ...`), a daemon failure after the
  send began and no answer (`no answer: ...`) exit 3: the agent was or may have been stopped, and a second run starts
  another new conversation. A session runs it only on its person's word, naming the session, and never for its own (the
  skills say so); the daemon refuses a session in a turn only while the registry is readable and the leg's hooks reach its
  own entry (rows' contract, Limits of `workspace.restart`), so the help and the skills say to run it when the session is
  at rest. `tests/test-sot-fe-restart.sh`, against a stub daemon, is in the Windows agents job of `release-gate.yml`.
- Still elsewhere, listed so a reader finds them: the shell daemon client `comm/lib/comm-lib-client.sh`
  (`sot_daemon_endpoint`, `sot_relay_endpoint`, `sot_oneshot_request`, `sot_pty_input`); the hooks' scripts, the Codex
  skills and plugin, and the two messaging skills (`sot-comm`, `sot-session-start`) in `comm/work_state/` and
  `comm/adapters/`: messaging's, installed into the layer by agents' installer stage.

## Calls in
The rows (`rows/spawn/detach.rs` calls `account_spawn_env` and `capsule_supervisor_env`; `rows/run/watchdog.rs`,
`rows/ops/create.rs` and `rows/run/activation.rs` call `agent_argv`; `rows/reauth` calls `claude_resume_argv`,
`account_env`, `discover_accounts` and `claude_config_dir`), the server's op table (`accounts.list`), `main.rs`
(`agent-exec`, `trust declare`, `own_sotd_bin` at start), the installer (`resolve_claude` for the Unix checklist;
`sotd trust declare`; `update_comm`'s adapter
stage for the layer and its retirements), `ccb` (`sotd agent-exec claude`), messaging's wake of a Codex row
(`agent_argv` for the launcher it runs with `--queue`, `NESTING_ENV_VARS_TO_SCRUB`, `env::CODEX_THREAD_FILE`).

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
`bin/ccx`, the row skill list `shared-skills.txt`. `spawn/` (the four row-lifecycle CLIs), `worktree/` (the four /worktree scripts), `sot-fe/` (the frontend and
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
| 4 | `env::tests::capsule_supervisor_env_*` (`capsule_supervisor_env_starts_claude_in_its_classic_renderer`), `nesting_env_scrub_list_is_pinned`, `every_row_leg_names_its_selection_file`, `a_verbatim_state_folder_gives_the_plain_forward_slash_form` (Windows only), `ccx_unsets_the_nesting_markers_the_scrub_list_names`, `tests/test-ccx-launch.sh` (`--capsule` keeps the selection file, a bare `ccx` drops it), `tests/capsule_workspaces/session_env.rs` (a real row's session names its own state folder's file), `rows::spawn::detach::start_mode_tests::the_scrub_comes_before_the_rows_pairs_so_the_row_keeps_its_selection_file`, `sotd_bin_value_*`, `the_leg_never_inherits_a_sotd_bin_its_own_start_path_rejected`; `tests/agent_exec.rs` |
| 5 | `accounts::tests::*`, `account_links::tests::*`; `tests/test-spawn-capsule-workspace.sh` ("no --account: a claude row gets the spawning session's account") |
| 6 | `folder_trust::tests::*` (`two_preparations_never_share_a_temporary_name`, `the_restart_asks_the_scope_the_record_uses`), `trust_declaration::tests::*`, `env::trust_declaration_controls::*`, `env::trust_scope_controls::*` (`a_recorded_or_already_trusted_preparation_is_one_info_line_naming_the_file`, `a_claude_preparation_marks_its_trust_file_and_no_other_file`), `env::trust_config_controls::*`; the layer: `folder_trust::tests::the_layer_is_recorded_wherever_it_lies_once_a_prefix_is_declared`, `the_layer_is_recorded_beside_what_the_file_already_holds`, `no_declaration_or_a_relative_one_records_no_layer`, `env::trust_scope_controls::a_declared_box_records_the_ready_layer_beside_the_row_and_an_undeclared_one_neither`, `a_layer_that_is_not_ready_is_not_recorded`, `the_layer_is_recorded_under_the_spelling_its_add_dir_carries` (Unix); `tests/trust_declare.rs`, `tests/trust_premises.rs` |
| 7 | `argv::tests::the_layer_rides_the_recipe_and_a_plain_claude_has_none`, `a_layer_without_the_bootstrap_skill_is_left_out_and_the_prompt_goes_with_it`, `the_layer_reaches_the_capsule_leg_the_reauth_leg_and_agent_exec`, `a_callers_settings_json_is_merged_with_the_recipes_hooks`, `layer::tests::*` (`the_selection_hook_rides_the_recipe`); `tests/agent_exec.rs::agent_exec_claude_carries_the_layer_when_installed`; `tests/test-hook-commands.sh` (all six Claude commands, including the selection command with its script present or missing, and the serialized Codex command draining a long prompt silently with the script or row path missing); `tests/test-selection-hook.sh` (the script's rows: unset, empty, missing, a directory, an empty file, the rendering byte for byte, a path with a space, unreadable, a 1 MiB input on stdin; laid flat and executable in the staged bin, and the serialized Codex command delivering the selection from the runtime comm home); `comm/tests/test-session-start-hook.sh` (the hook's output names the handle and the handoff, exits 0 with none, prints and joins nothing with hooks off); `a_recipe_hook_the_caller_already_has_is_not_added_twice`, `a_callers_settings_of_another_shape_is_passed_as_given`; `test/layer_tests.jl`, `test/protect_tests.jl` ("the mail check and the subagent guard stay in the user's settings"). A leg that carries the layer was proved at its argv, not by running Claude Code |
| 8 | `test/protect_tests.jl` (a person's skill, a symlinked skills folder, every configuration folder, a person's hook), `test/claude_hooks_tests.jl` (the settings retirement), `test/install_tests.jl` and `test/publish_tests.jl` (shared command preflight, replacement and cleanup) |
| 9 | `tests/test-tools-wire-only.sh`, `tests/test-spawn-remote-no-local-row.sh`, `tests/test-sot-fe-endpoint-once.sh`, `tests/test-sot-fe-chat.sh` (the chat ops), `tests/test-sot-fe-restart.sh` (`workspace.restart`) |
| 10 | `tests/test-ccb-agent-exec.sh`, `tests/test-ccx-launch.sh`, `tests/test-ccx-resume-default.sh` |
| 11 | the hosted run of `scripts/tests/under-bash.sh`; `test/skills_tests.jl` ("a launcher's Windows shim refuses at once and names Git Bash") |
| 12 | `tests/test-crlf-gh-auth.sh` |
| 13 | `test/install_tests.jl` (the agents testset; "comm-lib.sh and sot-fe install as one file each...") |
| 14 | `tests/test-temp-files.sh` |
| 15 | `tests/test-skill-templates.sh` (lints: `$digit`, frontmatter, the install page's lists) |
| 16 | `tests/test-spawn-capsule-workspace.sh` (a reader that leaves), `tests/test-despawn-resolve.sh` (D6b) |
