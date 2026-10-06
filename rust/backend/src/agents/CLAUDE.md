# rust/backend/src/agents: accounts, folder trust, awareness env and the launch recipe (backend agents)

What the daemon sets up around an agent's login before a capsule spawns: which named accounts exist, the folder-trust
record claude would otherwise ask about, the `SOT_*` awareness env, the argv each agent kind launches with, and the
env a spawn carries. Part of the backend's agents subsystem; charter:
agents/CLAUDE.md at the repo root (lands with the repo-root agents/ folder).

## Files
- `mod.rs`: declares the seven modules and the test support module.
- `accounts.rs`: account discovery, the account name rules, `account_env`, and the shared links a named account gets.
- `argv.rs`: the launch argv per agent kind (`agent_argv`, `agent_exec_argv`, `claude_resume_argv`) and the Unix path resolution of `claude` and `ccx`.
- `env.rs`: the spawn env (`capsule_supervisor_env`, `account_spawn_env` for the account preparation, `agent_env`) and the list of nesting variables scrubbed from every spawn.
- `folder_trust.rs`: the declared trusted-root prefix and the write of claude's per-folder trust record.
- `awareness.rs`: the `SOT_*` awareness env stamped on every capsule producer, and the daemon's own endpoint path.
- `memory.rs`: the auto-memory settings flag claude gets, and the proof that one shared store exists.
- `ops.rs`: accounts.list, and `sotd agent-exec`, the one owner of the launch recipe that `ccb` execs through
- `support_tests.rs`: `touch_dir`, `touch_file`, `platform_spelling` and the env guard, shared by the tests of the other files.

## Start here
`account_env` in `accounts.rs` for what a spawn sets for an account; `ensure_folder_trusted` in `folder_trust.rs` for the
trust record; `agent_argv` in `argv.rs` for a spawn's argv, and `agent_exec_argv` for `sotd agent-exec`, which `ccb`
execs.

## Rules
- A named account never shares the login: only the names in `SHARED_ENTRIES` are linked, by `ensure_account_links`.
- Folder trust is written only for a root under the declared prefix, and an entry already accepted is never rewritten
  (`ensure_folder_trusted`).
- The prefix comes only from the user-level settings file (`trusted_root_prefix`); no environment variable or project
  file declares it.
- Every claude launch passes `--permission-mode auto`, never `--dangerously-skip-permissions` (`claude_recipe`).
- On Unix `claude` and `ccx` resolve to absolute paths, because a daemon-spawned process lacks `~/.local/bin`
  (`resolve_claude`, `agent_env`).
- The auto-memory flag names a directory only when one shared store is proven, else no flag (`auto_memory_reason`).
- The variables in `NESTING_ENV_VARS_TO_SCRUB` are removed from every spawn and from `sotd agent-exec`.
- A session's environment gets `SOTD_BIN` from this daemon's argv[0], made absolute once at startup without following
  links (`capsule_supervisor_env`, `own_sotd_bin`). A bare name is searched in the working folder before `PATH` on
  Windows, with `.exe` added when it has no extension; on Unix only `PATH` is searched. A relative path is made
  absolute against the working folder, or that drive's working folder for a Windows drive-relative path. The value
  uses forward slashes on Windows and overrides any inherited value, so the comm shell bridges with the daemon's own
  binary. An empty start path, a failed absolute-path conversion or a bare name whose whole lookup finds no runnable
  file adds no value; the inherited value and the shell's own ladder then apply.
