# agents/tests: the suites that prove the agents CLIs (agents)

One standalone bash suite per `test-*.sh`, each against its own temporary comm home and a stub daemon on a unix socket,
never the live comm home or daemon. Part of agents; charter: agents/CLAUDE.md. The suites run from the folder, as CI
does (`working-directory: agents/tests`).

## Files
- `test-ccb-agent-exec.sh`: `ccb` execs `sotd agent-exec claude` with the caller's flags in order
- `test-ccx-launch.sh`: `ccx`'s default handle is built from the comm library's safe pieces and a failed derivation stops the launch
- `test-ccx-resume-default.sh`: `ccx` resumes by default on its `--capsule` flag alone, never an inherited variable
- `test-crlf-gh-auth.sh`: `sot-gh-auth.sh` reads JSON values correctly under a jq that writes CRLF
- `test-despawn-resolve.sh`: `comm-despawn.sh` resolves first, fails and changes nothing on an unknown name, removes a registry row only after a confirmed destroy; `comm-worktree-clean.sh` despawns once
- `test-sot-fe-reauth.sh`: `sot-fe reauth` moves only the row it runs in
- `test-sot-fe-version.sh`: `sot-fe version` asks the daemon what build it is
- `test-spawn-capsule-workspace.sh`: `comm-spawn.sh` never destroys a row it did not create
- `test-spawn-remote-no-local-row.sh`: a spawn onto another box writes no registry row or inbox here

## Start here
`test-spawn-capsule-workspace.sh` as the pattern: source the guard, make a work directory, `guard_fresh_home`,
`guard_stage_bin`, start the stub daemon, run the script from the stage.

## Rules
- Each suite sources `comm/tests/lib-home-guard.sh` before any command but `set`; `comm/tests/test-rm-guard.sh` fails a
  suite under `agents/` that does not.
- Scripts run from the copy `guard_stage_bin` makes in the suite's work directory, never from `agents/spawn/` or
  `agents/sot-fe/`; the two launcher suites run `ccb` and `ccx` from `agents/claude/bin` and `agents/codex/bin` through
  `$SCRIPT_DIR`.
