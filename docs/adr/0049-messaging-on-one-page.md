# ADR 0049: messaging on one page

**Status:** accepted as the design of record; supersedes ADR 0047 (ping wake) and ADR
0048 (filer receipts). Most of what follows is unbuilt: it lands in stages, and the
per-session watcher, listener and bridge machinery it replaces stays in place until
each stage does.

## Context

Messaging was redesigned repeatedly without removing the old parts, so several delivery
routes and wake paths ran at once, and the material a session loads at start
contradicted itself — a ruling that reaches nothing a session loads changes nothing.
The owner asked for the fix as "agree on the one page comm system and then cleanup".

## Decision

- **Address** — a handle is a session's repository or worktree folder name plus its box
  name (`myrepo-laptop`); two boxes never share one. A row's session declares its
  handle to its daemon when it first starts; the daemon keeps it through restarts,
  compactions and clears, gives each handle to one row only, and a newer declaration
  moves it.
- **Inbox** — one file per handle, `inbox/<handle>.jsonl`, in the box's comm folder
  (`~/.sot-comm`, or `SOT_COMM_HOME`). The read cursor is a line count; unread mail is
  any line past it addressed to this handle by someone else.
- **Sending** — two routes, by whether the sender's own comm folder lists the receiver:
  file it itself, or hand it to the hub (the one daemon every box can reach), which
  offers it to every linked daemon, and the one holding that inbox files it and says so.
  A daemon on a box with its own disk keeps its own link to the hub, opened when it
  starts and reopened if it drops, so the box is reachable whenever its rows run,
  window open or not; its sessions send through that same link. A liveness check runs
  first — a row still running that handle, or active in the last ten minutes.
- **The one result** — `filed -> @h` (exit 0), or `FAILED -> @h: <reason>` (exit 1): no
  box knows the handle, no live session holds it, the hub is unreachable, or no daemon
  says "filed" within 5 seconds. Nothing is queued; there is no second route.
- **Waking** — every two seconds each daemon looks at every row it runs, and types one
  fixed line into a row with unread mail sitting at a free prompt — the cursor at the
  start of an empty input line; a dialog, menu, draft or working session is not free
  and is never typed into. One line per batch, one more after ten minutes unread. A
  busy session needs no typing — its end-of-turn check will not let a turn finish with
  unread mail waiting. This is the only wake: no per-session watcher, listener, bridge
  or Monitor exists.
- **Cases** — a restarted or compacted row re-arms nothing, since the handle stays with
  the row and the count is a file. A session outside any row sees mail only at its own
  next turn end while idle, and after ten idle minutes a send to it fails. A subagent
  sends under its parent's handle and never reads the inbox itself.

## Why the daemon and not the frontend

- The daemon types, not the frontend, because several frontends can show one row and
  each would type, and a closed window would leave the row deaf — put to the owner and
  approved; the first shape had the frontend check.
- The session reads the message through `comm-poll.sh` rather than having it pasted,
  because a pasted message is never marked read, so it would show again, hold the next
  turn open, and read as the owner's own words — put to the owner and approved; the
  first shape pasted the message.
- The check repeats every two seconds rather than once at filing time, because one try
  misses a row that is busy, compacting or restarting at that moment.

## Consequences

A session is told all of this at start: `comm-context.sh` prints its handle; send with
`comm-send.sh @handle "text"` and read the one result; when `[sot-comm] you have mail`
appears, or the end-of-turn check says so, run `comm-poll.sh`; to wait for a reply, end
the turn. A frontend plays no part in messaging, and a session on a box with no daemon
is not woken while idle. ADRs 0047 and 0048 are superseded.
