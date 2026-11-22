Part of messaging: see ../CLAUDE.md.

`comm-session-start-hook.sh` is the Claude Code SessionStart hook (`clear` and `compact`) a row's leg carries: it runs
`comm-session-start.sh --again`, the read-only start. The messaging contract says what it prints and guarantees.

The Codex row plugin wraps the same read-only `comm-session-start.sh --again` for clear, compaction and resume.
Only successful start lines become model context. The pinned identity, inbox cursor and open question survive.
