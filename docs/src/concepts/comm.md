# The comm relay

The comm relay is how sessions find and talk to each other, on one machine or
across many. It has two halves with different jobs.

**Data at rest: the registry and the inboxes.** Under `~/.sot-comm/` each host
keeps a `registry.json` (every session's handle, host, workspace, and its
work-state facts) and one append-only `inbox/<handle>.jsonl` per session.
Discovery, catch-up after a sleep, and the work-state colours all read these
files; nothing about them needs a live broker.

**Live delivery: the daemon.** An instant message rides the Ship of Tools
daemon — the same connection the frontend already uses. `comm-relay.sh send`
hands the frame to the daemon, which broadcasts it to its connected clients;
each host's listener files the frames addressed to its sessions into their
inboxes and wakes the recipient if it is idle at its prompt. Because the relay
reuses the backend connection, a message from a GPU server reaches a session
on your laptop with no extra port or service.

The relay is **live-only** by design: the daemon forwards frames, it does not
queue them. What makes delivery durable is the inbox on the receiving side,
and what proves delivery is a reply.

## The contract

`comm/PROTOCOL.md` is the contract every client implements — the Claude Code
skill, the Codex adapter, and any future client — so all of them are mutually
addressable through the same registry and inboxes. It is published here as the
[Comm protocol](../ref/comm.md) reference.

For day-to-day use, see [Agents messaging each other](../guide/messaging.md).
