# The comm relay

The comm relay is how sessions find and talk to each other, on one machine or
across many. It has two halves with different jobs.

**Data at rest: the registry and the inboxes.** Under `~/.sot-comm/` each host
keeps a `registry.json` (every session's handle, host, workspace, and its
work-state facts) and one append-only `inbox/<handle>.jsonl` per session.
Discovery, catch-up after a sleep, and the work-state colours all read these
files; nothing about them needs a live broker.

**Delivery is a file.** A send to a handle this host's registry names is
appended to that handle's inbox directly only when this host provably shares
the hub's lock on `~/.sot-comm/`. Otherwise the frame goes to a daemon as
`comm.file`, and a daemon that is not the folder's hub hands it to the hub,
which files it. Either way the send answers `filed -> @handle`, which is the
acknowledgement: **the file is the ack.** A filed frame is read by the recipient's next turn boundary,
because the recipient's own end-of-turn hook reads its inbox and will not let
the turn end while directed mail sits unread. That is also how a BUSY session
is reached — no process, no keystrokes, nobody to ask.

A session sitting idle at its prompt is additionally *poked*: one gated line
typed into its row, because a stopped agent is blocked on stdin and keystrokes
are the only way in. The poke is a shortcut, never the delivery — `+woken` or
`not woken: <reason>` is a diagnostic on the verdict, not the verdict.

**The wire: the daemon.** For a handle this host cannot name — one on another
box — the frame rides the Ship of Tools daemon, the same connection the
frontend already uses. The hub files it itself when its own comm folder holds
that handle; any other handle is still offered to the receiving side's
listener (or, on Windows, the frontend itself), which files it. Because the relay reuses the backend connection, a
message from a GPU server reaches a session on your laptop with no extra port
or service. The daemon does not queue frames, so a send nothing can file is
reported as a failure — `FAILED -> @h: …` — instead of an ack for the daemon's
own success.

## The contract

`comm/PROTOCOL.md` is the contract every client implements — the Claude Code
skill, the Codex adapter, and any future client — so all of them are mutually
addressable through the same registry and inboxes. It is published here as the
[Comm protocol](../ref/comm.md) reference.

For day-to-day use, see [Agents messaging each other](../guide/messaging.md).
