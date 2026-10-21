# Agents messaging each other

Sessions talk to each other. An agent working on your package can ask the
agent working on its dependency a question, a worktree session can report back
to its parent, and a session on a GPU server can tell the one on your laptop
that a run finished. This is the **comm relay**; the agents use it through the
bundled skills, and you can use the same scripts from any shell.

## Every session joins at start

A Claude Code or Codex session created in Sessions mode runs
`/sot-session-start` once: it joins the registry under a **handle** — its
row's name — and starts listening. From then on the session is addressable as
`@<handle>` from any other session, on any connected host.

## Ask, tell, broadcast

The agents call these for you when you say "ask the kernel session whether…",
but they are plain scripts in `~/.sot-comm/bin`:

```bash
comm-list.sh                                 # who is reachable, and their state
comm-send.sh @other-session "message"        # one result: filed, or FAILED with the reason
comm-send.sh --broadcast "message"           # filed to every live session, one result each
comm-send.sh @other-session --file notes.md  # the body is the file's bytes: quotes and $( ) arrive as written
comm-send.sh @other-session - < notes.md     # the body on stdin, the same way
comm-poll.sh                                 # catch up on anything that arrived while you were away
```

Each script runs `sotd comm`. A send appends the message to the recipient's
inbox (`~/.sot-comm/inbox/<handle>.jsonl`), itself or through the Ship of Tools
daemon that owns that folder, and says `filed` only once the line is there; a
session that was busy or away reads it with `comm-poll.sh` when it gets to it.

## A message wakes the recipient

An idle session does not have to be polling. When a message lands for a
session at its prompt, it is typed into that session's row. A Claude Code
session gets one short wake line and reads the messages itself with
`comm-poll.sh`, so a burst of messages costs one wake. A Codex session is not
typed into (no Codex screen has been captured yet), so it reads its mail when it
next runs `comm-poll.sh`. The wake is refused while the row shows a
permission dialog or a menu, so it can never answer one by accident, and while
the prompt holds anything you have typed and not sent, so it never touches your
line.

## What travels with it

The same registry that routes messages carries each session's work state
(working, question, waiting, done, idle), which is what colours the rows in
Sessions mode. See [Work-state colours](../concepts/work-state.md) and, for the
wire format and the registry layout, [The comm relay](../concepts/comm.md) and
the [Comm protocol](../ref/comm.md) reference.
