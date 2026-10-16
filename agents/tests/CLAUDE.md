# agents/tests: the suites that prove the agents CLIs (agents)

One standalone bash suite per `test-*.sh`, each against its own temporary comm home and a stub daemon on a unix socket,
never the live comm home or daemon. Part of agents; charter: agents/CLAUDE.md. The suites run from the folder, as CI
does (`working-directory: agents/tests`).

## Files
- `test-ccb-agent-exec.sh`: `ccb` execs `sotd agent-exec claude` with the caller's flags in order
- `test-ccx-launch.sh`: `ccx`'s default handle is built from the comm library's safe pieces and a failed derivation stops the launch
- `test-ccx-resume-default.sh`: every `ccx` starts fresh and resumes only on an explicit `--continue`, whatever its environment; `--capsule` changes nothing; the first prompt names the session's handoff when one exists
- `test-crlf-gh-auth.sh`: `sot-gh-auth.sh` reads JSON values correctly under a jq that writes CRLF, every curl call carries the stall bound, and a gh call that hangs ends at the script's 30 s bound with a line naming it (about 31 s)
- `test-despawn-resolve.sh`: `comm-despawn.sh` resolves first, fails and changes nothing on an unknown name, removes a registry row only after a confirmed destroy; `comm-worktree-clean.sh` despawns once
- `test-sot-fe-goto.sh`: `sot-fe goto` has no `--boot`: the flag is refused as unknown and nothing is sent
- `test-sot-fe-paths.sh`: an absolute path given to `sot-fe preview` or `sot-nav.sh preview` is sent relative to its
  root in every spelling Windows or a link above the root gives one folder (on Windows in any case, also a root typed
  in another case with the path through a link), a link inside the root kept as written (`sot_relative_to`), a
  self-file root in the shell's spelling and case is the project's whose root git spells (`sot_same_root`, through
  `comm-context.sh`), Linux compares case as written, `sot_daemon_path` keeps its contract, and the path `sot-fe docs`,
  `sot-fe repl run` and comm-probe's `workspace.create` send a `pipe:` daemon is in its spelling;
  `sot-fe repl run --fresh` sends no `eval_id` (the daemon numbers and announces the run) and sends its `--origin`
- `test-sot-fe-reauth.sh`: `sot-fe reauth` moves only the row it runs in
- `test-sot-fe-version.sh`: `sot-fe version` asks the daemon what build it is
- `test-spawn-capsule-workspace.sh`: `comm-spawn.sh` never destroys a row it did not create; a bash row (`--agent none`) claims no handle and writes no registry row or inbox
- `test-spawn-remote-no-local-row.sh`: a spawn onto another box writes no registry row or inbox here
- `test-temp-files.sh`: no agents script leaves a file in its temporary folders, on success, on failure, killed, or ended by TERM, HUP or INT; a signal ends `sot-gh-auth.sh poll`, and so does the code's expiry with the network down; with the signal sent twice (during the trap's rm, or back to back), the poll's state file, comm-spawn's rollback, scripts/install.sh's prelude copy (every host) and download folder (Linux x86_64, macOS
  arm64) still go, and on Linux docs/tools/docs-media.sh's work folder; a Ctrl-C ends docs-media.sh even when its
  child catches it, and a Ctrl-C during comm-spawn.sh stops the shell that ran it; comm-spawn's
  rollback says it left a row that changed meanwhile, on a Ctrl-C and on a failed create; the media tool's rows run on
  the suite's own bash, when that is 4 or later

## Start here
`test-spawn-capsule-workspace.sh` as the pattern: source the guard, make a work directory, `guard_fresh_home`,
`guard_stage_bin`, start the stub daemon, run the script from the stage.

## Rules
- Each suite sources `comm/tests/lib-home-guard.sh` before any command but `set`; `comm/tests/test-rm-guard.sh` fails a
  suite under `agents/` that does not.
- The job `bash32-linux` runs every suite here with every script under bash 3.2 (`scripts/tests/under-bash.sh`); code a
  suite hands to the bash on PATH holds under 3.2 (comm/tests/CLAUDE.md).
- The stub daemon's socket is awaited with `await` (comm/tests/lib-wait.sh), signalled by the socket itself, never by a
  fixed wall-clock wait. Its two steps, `nc` and the `tail -F | while` watcher, start between `set -m` and `set +m` and
  end with `end_group`, in `stop_stub_daemon` and in the EXIT trap (comm/tests/CLAUDE.md).
- Scripts run from the copy `guard_stage_bin` makes in the suite's work directory, never from `agents/spawn/` or
  `agents/sot-fe/`; the two launcher suites run `ccb` and `ccx` from `agents/claude/bin` and `agents/codex/bin` through
  `$SCRIPT_DIR`.
