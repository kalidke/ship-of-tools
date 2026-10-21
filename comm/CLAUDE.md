# comm/: messaging's shell side

Messaging's contract is `rust/backend/src/comm/CLAUDE.md`; the on-disk formats and the wire are `PROTOCOL.md` here.
Messaging itself is `sotd comm`; what lives here is the shell every installed script shares.

## Rules for the scripts here and the ones that source the library (agents', rows')
- They run under bash 3.2 and Git Bash: `scripts/tests/under-bash.sh` runs every hermetic suite under GNU bash 3.2.57
  in CI (`bash32-linux`) and under macOS's `/bin/bash`.
- Free text, and any value that may start with `/`, reaches jq on stdin and a native program as data, never as an
  argument Git Bash would rewrite (`comm/lib/comm-lib-base.sh`, free text and jq).
- A cleanup trap (an EXIT trap that removes or undoes what its shell holds) starts `trap "" HUP INT TERM`, ends with
  `sot_end_by_signal`, and its shell runs `sot_cleanup_signals` right after setting it (`comm/lib/comm-lib-base.sh`,
  cleanup traps); a test suite's own traps are the suite's.
- A messaging function a script calls is the shell door's (`lib/comm-lib-sotd.sh`), which runs `sotd comm`: no script
  reads or writes the comm folder's registry, inboxes or cursors itself.

## Folders
- `adapters/`: what is installed into Claude Code and Codex: the messaging skills, the Codex hooks and plugin.
- `lib/`: the shell library every installed script sources (`comm-lib.sh`): its base helpers, the daemon client the
  agents' CLIs use (`comm-lib-client.sh`), and messaging's shell door to `sotd comm` (`comm-lib-sotd.sh`).
- `mail/`, `registry/`: the commands sessions type, thin wrappers over `sotd comm`.
- `tests/`: the shell-side checks and the suites' shared harness (the home guard, the stager, the waits).
- `work_state/`: rows' work-state scripts and hooks (rust/backend/src/rows/CLAUDE.md); its subagent guard is
  messaging's.
- `bin-folders.txt`: the folders whose files install flat into `~/.sot-comm/bin`, in publish order (`lib` first).
