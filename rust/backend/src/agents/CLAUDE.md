# rust/backend/src/agents: accounts, folder trust, awareness env and the launch recipe (backend agents)

What the daemon sets up around an agent's login before a capsule spawns: which named accounts exist, the folder-trust
record claude would otherwise ask about, the `SOT_*` awareness env, the argv each agent kind launches with, and the
env a spawn carries. Part of the backend's agents subsystem; charter:
agents/CLAUDE.md at the repo root (lands with the repo-root agents/ folder).

## Files
- `mod.rs`: declares the agent modules and test support module.
- `accounts.rs`: account discovery, the account name rules, `account_env`, and the shared links a named account gets.
- `argv.rs`: the launch argv per agent kind (`agent_argv`, `agent_exec_argv`, `claude_resume_argv`) and the Unix path resolution of `claude` and `ccx`.
- `env.rs`: spawn environment, account preparation and effective Claude trust-file selection.
- `folder_trust.rs`: resolved scope, child project keys, trust outcomes and trust-file mutation.
- `folder_trust_tests.rs`: observable scope, key, JSON preservation and no-write behavior.
- `trust_declaration.rs`: the typed user-level trust declaration and the offline declaration command.
- `trust_declaration_tests.rs`: parsed declaration, preservation and argument behavior.
- `awareness.rs`: the `SOT_*` awareness env stamped on every capsule producer, and the daemon's own endpoint path.
- `memory.rs`: the auto-memory settings flag claude gets, and the proof that one shared store exists.
- `ops.rs`: accounts.list, and `sotd agent-exec`, the one owner of the launch recipe that `ccb` execs through
- `support_tests.rs`: `touch_dir`, `touch_file`, `platform_spelling` and the env guard, shared by the tests of the other files, with config-directory restoration.

## Start here
`account_env` in `accounts.rs` for what a spawn sets for an account; `ensure_folder_trusted` in `folder_trust.rs` for the
trust record; `agent_argv` in `argv.rs` for a spawn's argv, and `agent_exec_argv` for `sotd agent-exec`, which `ccb`
execs.

## Rules
- A named account never shares the login: only the names in `SHARED_ENTRIES` are linked, by `ensure_account_links`.
- Folder trust is recorded only when the OS-resolved root lies under the OS-resolved declared prefix. The key uses the child cwd's spelling; an already accepted entry is not rewritten. Outside, undeclared and failed preparation are observable and the agent still starts. The trust file follows the child's effective `CLAUDE_CONFIG_DIR`: account additions override inheritance; with no config override it is the home-level file. Automated evidence establishes preparation; actual child-config consumption remains a human done-test proof limit. Interactive trust recognition, parent coverage and no-dialog behavior remain outside the headless proof.
- The prefix is parsed as TOML from the user-level settings file at each spawn; no environment variable or project file
  declares it. Invalid input is diagnostic and declares no trust.
- Trust preparation has no conditional replacement guarantee for concurrent external edits of existing settings or state files;
  W1 keeps the existing update writers. This is W3's classified 0.6.7 hardening limit. Missing-file creation uses the existing
  platform no-replace publication; an appearing destination is kept.
- P0/P1/P5 are proof limits: real-Claude config consumption, child-observed spelling and unusual config semantics are not checked.
  Interactive recognition, parent coverage and no-dialog behavior require the person-run release done test.
- A Windows alias case whose facility is missing (a drive-letter, distinct drive, short-name or junction spelling) fails
  on CI, which GitHub marks with `CI`, naming what was missing; elsewhere it prints its named NOT CHECKED
  (`not_checked` in `folder_trust_tests.rs`). The C5 unavailable control's drive-alias NOT CHECKED is planned and stays.
- Every claude launch passes `--permission-mode auto`, never `--dangerously-skip-permissions` (`claude_recipe`).
- The daemon's own `PATH` need not hold `~/.local/bin`, so on Unix `claude` and `ccx` resolve to absolute paths, from
  `PATH` and then `~/.local/bin` (`resolve_claude`, `resolve_ccx`), and `capsule_supervisor_env` puts `~/.local/bin` at
  the front of a leg's `PATH` that lacks it (`agent_env`; `sotd agent-exec` too).
- The auto-memory flag names a directory only when one shared store is proven, else no flag (`auto_memory_reason`).
- The variables in `NESTING_ENV_VARS_TO_SCRUB` are removed from every spawn and from `sotd agent-exec`.
- A session's environment gets `SOTD_BIN` from this daemon's argv[0], made absolute once at startup without following
  links (`capsule_supervisor_env`, `own_sotd_bin`). A bare name is searched in the working folder before `PATH` on
  Windows, with `.exe` added when it has no extension; on Unix only `PATH` is searched. A relative path is made
  absolute against the working folder, or that drive's working folder for a Windows drive-relative path. The value
  uses forward slashes on Windows and overrides any inherited value, so the comm shell bridges with the daemon's own
  binary. An empty start path, a failed absolute-path conversion or a bare name whose whole lookup finds no runnable
  file adds no value; the inherited value and the shell's own ladder then apply.
