# The comm relay

The comm relay is how sessions find and talk to each other, on one machine or
across many. It has two halves with different jobs.

**Data at rest: the registry and the inboxes.** Under `~/.sot-comm/` each host
keeps a `registry.json` (every session's handle, host, workspace, and its
work-state facts) and one append-only `inbox/<handle>.jsonl` per session.
Discovery, catch-up after a sleep, and the work-state colours all read these
files; nothing about them needs a live broker.

**Delivery is a file.** A send to a handle this host's registry names is a
plain append to that handle's inbox — the two sides share `~/.sot-comm/`, so
there is no broker in the path at all. `comm-relay.sh send` takes that route
itself and answers `filed -> @handle`, which is the acknowledgement: **the file
is the ack.** A filed frame is read by the recipient's next turn boundary,
because the recipient's own end-of-turn hook reads its inbox and will not let
the turn end while directed mail sits unread. That is also how a BUSY session
is reached — no process, no keystrokes, nobody to ask.

A session sitting idle at its prompt is additionally *poked*: one gated line
typed into its row, because a stopped agent is blocked on stdin and keystrokes
are the only way in. The poke is a shortcut, never the delivery — `+woken` or
`not woken: <reason>` is a diagnostic on the verdict, not the verdict.

**The wire: the daemon.** For a handle this host cannot name — one on another
box — the frame rides the Ship of Tools daemon, the same connection the
frontend already uses, and the receiving side's listener (or, on Windows, the
frontend itself) files it. Because the relay reuses the backend connection, a
message from a GPU server reaches a session on your laptop with no extra port
or service. The daemon forwards frames and does not queue them, so a send it
cannot file for anyone is reported as a failure — `no such handle` — instead of
an ack for the daemon's own success.

## On Windows the frontend is the mail carrier

On Windows the attached frontend files cross-machine mail itself, straight
into its own inbox, so that machine runs no relay bridge at all. That has one
consequence worth knowing before you rely on it:

- **Same-machine sends always land.** A handle that machine's own registry
  names is a plain file append, frontend or no frontend.
- **Cross-machine mail needs the frontend running.** Close the window and a
  frame addressed to a session on that machine is filed nowhere. It is not
  queued for later.
- **The sender is told.** In that case the send answers `NOT CONFIRMED`, not a
  quiet success, so nobody is left believing a message arrived. See
  [When a send does not land](../guide/messaging.md).

A session on that machine still reads everything already in its inbox at its
next turn boundary; what a downed frontend costs is the *arrival*, not the
catch-up.

## Believing what comes back

The relay compares identifiers it reads out of JSON frames — handles, hosts,
workspace ids, cursor offsets. A Windows `jq.exe` writes its output in text
mode, which turns every newline it emits into a carriage return plus a
newline, and the shell idioms that capture a value keep that carriage return
glued to it. A handle read back that way never compares equal to the clean
handle it should match, so a machine could report `no such handle` for a
delivery that had already landed.

The identifier reads on the paths that produce a verdict — a handle, a host, a
workspace id, a registry root — go through a helper that strips that carriage
return where the value is read. The helper is applied call site by call site
rather than enforced by the language, so a read that has not been converted
yet can still carry one: a stray `no such handle` on Windows is worth checking
against that before anything else.

Free text — a message body, a status line somebody wrote — is deliberately
left alone, because stripping characters there would silently rewrite what the
sender typed instead of fixing a comparison.

The cross-machine [acceptance matrix](../contributing.md) is what checks this
end to end.

## The contract

`comm/PROTOCOL.md` is the contract every client implements — the Claude Code
skill, the Codex adapter, and any future client — so all of them are mutually
addressable through the same registry and inboxes. It is published here as the
[Comm protocol](../ref/comm.md) reference.

For day-to-day use, see [Agents messaging each other](../guide/messaging.md).
