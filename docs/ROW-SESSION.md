# Working from a Ship of Tools row

The root `CLAUDE.md` sends a session that runs in a row here: this page holds the row and messaging tooling such a
session uses.

## Working in this repo
- Development runs in the `ship-of-tools` workspace row. The daemon's default row is a hidden anchor; nothing runs in
  it.
- `/worktree` (`agents/worktree/comm-worktree-new.sh <short>`) makes `<repo-parent>/worktrees/ship-of-tools-wt-<short>`
  on branch `wt/<short>` with its own session; `/worktree status|sync|clean` manage it.
- Window restart (ADR 0017; read it before any restart): never kill the window's process. `scripts/relaunch-sot.ps1`
  writes the relaunch sentinel and the Windows launcher respawns the window on exit 75 or 76. On Linux and macOS the
  installed all-in-one `sot-launch` respawns on 75 only, and a window started by `scripts/launch-sot.sh` is not
  respawned.
- Releases follow the `release` skill and `scripts/release.sh`.
- A session's handoff is its recovery file, `dev/output/handoff-<handle>.md` (gitignored), written at milestones.

## Messaging between sessions
- Design of record: `docs/adr/0049-messaging-on-one-page.md`; the contract is `comm/PROTOCOL.md`.
- A session's handle is its folder name plus its box name; `comm-context.sh` prints it.
- Send with `comm-send.sh @<handle> "text"`; its one result is `filed -> @<handle>` or `FAILED -> @<handle>: <why>`,
  except that a send relayed to a handle the hub's folder does not list can still end `NOT CONFIRMED: sent for @h; ...`
  or `filed -> @h (by <filer>, relay)` (`comm/mail/comm-relay.sh`; known limit B2, in `comm/PROTOCOL.md`'s
  Delivery section).
- An idle row is woken by the line `[sot-comm] you have mail: run comm-poll.sh`; a turn cannot end with directed
  mail unread. Read with `comm-poll.sh`. To wait for a reply, end your turn.
- Run `/sot-session-start` once when a session starts.
- A subagent you launch uses no mail: its brief says nothing of mail, it reports to you, and you send.
