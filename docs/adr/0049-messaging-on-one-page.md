# ADR 0049: messaging on one page

**Status:** current — accepted as the design of record; supersedes ADR 0047 (ping wake) and ADR
0048 (filer receipts). Part of what follows is built (the daemon's wake, the inbox lock); the
relay's single verdict (lane M4) is not yet. The rest lands in stages, and the
per-session watcher, listener and bridge machinery it replaces stays in place until
each stage does.

2026-10-04: User isolation added (release captain's ruling); decision 0031 holds the
guarantees, this ADR the design.

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
  files it itself, or hands it to the hub (the one daemon every box can reach), which
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
  that runs inside its parent's agent process is part of that session: it may send under
  the parent's handle and never reads the inbox. Another agent started inside a session
  (`codex exec`, `claude -p`), and anything it starts, has no comm identity: a process
  acts as a handle only if at most one agent lies between it and its row's capsule, or
  the top of its process tree outside a row; an ancestry that cannot be read in full is
  refused. An agent that needs its own handle is started as its own row. The rule, and
  what the check does not see, are in `comm/PROTOCOL.md`.

## User isolation

Decision 0031's isolation guarantee (amended 2026-09-29) is the requirement, and this
section is the design built to it; where the two differ, the guarantee wins.

- **Users on one computer never see each other.** Nothing one OS user runs can reach,
  or be reached by, another OS user's daemon, hub, tunnel, pipe, port, inbox or
  staging folder. A process of another account that connects to a user's session
  socket or pipe, a hub relay socket, a page server or the page proxy is refused
  before anything is served to it, and a client never talks to a daemon pipe that
  another account serves.
- **The control plane uses no loopback ports.** The daemon, the hub, the relay and
  the pipes listen and dial only through Unix sockets in a private directory,
  owner-only named pipes and ssh logins, never a TCP port, so the operating system's
  login decides who connects.
- **Page servers and the page proxy serve only their own account.** A browser reaches
  a page only over TCP, so each of these listeners asks the operating system which
  account owns an accepted connection and drops it, before reading a byte, unless the
  account is its own. The URL's secret stays as a second lock, because the owner check
  admits every page the user's own browser loads.
- **Two OS users on one hub account get a loud refusal.** When a second OS user's hello
  reaches a hub for a host that a first OS user already holds through the same hub
  account, the hub refuses it with an error reply and closes the connection; nothing is
  ever filed for one user into the other's inbox, and the first user's connections are
  left alone.

The cost is one ssh login per frontend connection, attached lane and proxied page
connection.

**Status.** Built: the control plane listens on no TCP port. The daemon's one listener
is its session socket, whose directory must be private, or on Windows a named pipe
with an owner-only descriptor, and a hub's per-host relay sockets are owner-only Unix
sockets. Not built: the daemon serves its ops and events, mail included, to a
connection that has sent no hello; the hello names no OS account; and the peer read at
accept refuses another account only for a lease, so a hub cannot tell two OS users on
one hub account apart. Lane M1 builds the hello admission and that refusal, and deletes
`LaneDial::Tcp`, a TCP lane dial that only tests construct. On Windows the frontend,
its lease, `sotd stdio-bridge` and the lane client connect to whatever answers their
pipe name, a name in the machine-wide pipe namespace, without checking which account
serves it; lane M1b builds that check. The video, site and site-pool servers, the frontend's page proxy and the one-use
redirect listener that opens a page in the browser accept only through `serve_own`,
which drops another account's connection before reading a byte. Pluto's server, its
notebook workers and `wglshow`'s Bonito server are Julia processes that listen on ports of
their own, which any account can reach directly. Ship of Tools does not accept on them, so
no owner check reaches them; each is locked by a secret instead, and the guarantee is that
the secret never reaches another account (not its command lines, files or logs): Pluto's
page by its session secret, `wglshow`'s page by its secret path and its websocket by a
session id drawn from the OS's secure generator, and every Pluto notebook worker by the
Distributed cluster cookie (16 characters drawn from the OS's secure generator), which the
worker reads from its stdin and checks on every connection before it reads a message (Pluto's default Malt worker accepted the first
connection with no secret, and is not used: `julia/pluto/session_options.jl`, tested on
Linux, macOS and Windows by `julia/pluto/test/runtests.jl`). One door stays open: Bonito's
asset route on `wglshow`'s port serves the files a figure's session registers, keyed by a
hash of each file's path, to any account that can name the path (public library files, and
any file a figure names with `Asset(path)`); Bonito has no way to put it behind the secret
and an asset server that embeds files in the page is not selectable for an app route in
Bonito 5.1, so the limit is recorded, not closed. The cost is on Windows only: in this mode
Pluto cannot stop a running cell there (it says so; restoring interrupt is planned for
0.6.7). The guarantee is isolation, not availability: another account can still fill a
listener's backlog and delay this account's own connections; each connection it opens is
refused quickly.
The comm scripts create the comm folder and its inboxes with no mode of
their own, so these are only as private as the creating shell's umask and the home
folder above them; no lane is named for that yet.

## Why the daemon and not the frontend

- The daemon types, not the frontend, because several frontends can show one row and
  each would type, and a closed window would leave the row deaf — put to the owner and
  approved.
- The session reads the message through `comm-poll.sh` rather than having it pasted,
  because a pasted message is never marked read, so it would show again, hold the next
  turn open, and read as the owner's own words — put to the owner and approved.
- The check repeats every two seconds rather than once at filing time, because one try
  misses a row that is busy, compacting or restarting at that moment.

## Consequences

A session is told all of this at start: `comm-context.sh` prints its handle; send with
`comm-send.sh @handle "text"` and read the one result; when `[sot-comm] you have mail`
appears, or the end-of-turn check says so, run `comm-poll.sh`; to wait for a reply, end
the turn. A frontend plays no part in messaging, and a session on a box with no daemon
is not woken while idle.
