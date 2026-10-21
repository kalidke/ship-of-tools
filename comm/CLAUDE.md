# comm/: messaging's shell side (pointer)

Messaging's contract is `rust/backend/src/comm/CLAUDE.md`; the on-disk formats and the wire are `PROTOCOL.md` here.

## Folders
- `adapters/`: what is installed into Claude Code and Codex: the messaging skills, the Codex hooks and plugin.
- `lib/`: the shell library every installed script sources (`comm-lib.sh`), with messaging's shell door to `sotd comm`.
- `mail/`, `registry/`: the commands sessions type, thin wrappers over `sotd comm`.
- `tests/`: the shell-side checks.
- `work_state/`: rows' work-state scripts and hooks (rust/backend/src/rows/CLAUDE.md); its subagent guard is
  messaging's.
- `bin-folders.txt`: the folders whose files install flat into `~/.sot-comm/bin`, in publish order (`lib` first).
