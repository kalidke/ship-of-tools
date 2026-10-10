# Working in this repo as a session

The root `CLAUDE.md` sends every session here, in a row or outside any.

## Working in this repo
- Development runs in the `ship-of-tools` workspace row. The daemon's default row is a hidden anchor; nothing runs in
  it.
- `/worktree` (`agents/worktree/comm-worktree-new.sh <short>`) makes `<repo-parent>/worktrees/ship-of-tools-wt-<short>`
  on branch `wt/<short>` with its own session; `/worktree status|sync|clean` manage it.
- Window restart (ADR 0017; read it before any restart): `scripts/relaunch-sot.ps1` writes the relaunch sentinel and
  the Windows launcher respawns the window on exit 75 or 76. On Linux and macOS the installed all-in-one `sot-launch`
  respawns on 75 only, and a window started by `scripts/launch-sot.sh` is not respawned.
- Releases follow the `release` skill and `scripts/release.sh`.
- A session's handoff is its recovery file, `dev/output/handoff-<handle>.md` (gitignored), written at milestones.

## Messaging between sessions
- Run `/sot-session-start` once when a session starts; its skill, `comm/adapters/claude/sot-session-start/SKILL.md`,
  says how to send, read and wait. The design of record is `docs/adr/0049-messaging-on-one-page.md`; the contract,
  with its known limits, is `comm/PROTOCOL.md`.
- A subagent you launch uses no mail: nothing in its brief tells it to use mail; it reports to you, and you send.
