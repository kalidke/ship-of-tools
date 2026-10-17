# Agents messaging each other

Sessions talk to each other. An agent working on your package can ask the
agent working on its dependency a question, a worktree session can report back
to its parent, and a session on a GPU server can tell the one on your laptop
that a run finished. This is the **comm relay**; the agents use it through the
bundled skills. You can use the same scripts from a shell that has joined (a row's own shell has; the Terminal
drawer or a bare terminal has not, and a send from there is refused).

## Every session joins at start

A Claude Code session created in Sessions mode runs `/sot-session-start` once; a Codex row is joined by its launcher.
Either way the session joins the registry under a **handle**, made from its folder and box name; `comm-list.sh`
prints the exact string. From then on the session is addressable as `@<handle>` from any other session, on any
connected host.

## Ask, tell, broadcast

The agents call these for you when you say "ask the kernel session whether…",
but they are plain scripts in `~/.sot-comm/bin`:

```bash
comm-list.sh                                 # who is reachable, and their state
comm-send.sh @other-session 'message'        # one result: filed, or FAILED with the reason
comm-send.sh --broadcast 'message'           # filed to every live session, one result each
comm-send.sh @other-session --file notes.md  # the body is the file's bytes: quotes and $( ) arrive as written
comm-poll.sh                                 # read what has arrived
```

Quote the message in single quotes: inside double quotes the shell runs backquotes and `$( )` in it as commands
before the send.

A send appends one line to the recipient's inbox (`~/.sot-comm/inbox/<handle>.jsonl`) and prints `filed -> @handle`.
It fails, and files nothing, when no live session holds the handle. The recipient reads it with `comm-poll.sh`.

## A message wakes the recipient

An idle Claude Code session does not have to be polling. When a directed message is filed for a session sitting at a
free prompt, the daemon types one fixed line into its row (`[sot-comm] you have mail: run comm-poll.sh`), and the
session reads the messages itself with `comm-poll.sh`, so a burst of messages costs one wake. The wake is refused
while the row shows a permission dialog or a menu, so it can never answer one by accident. A Codex session is not
woken: it sees mail when it runs `comm-poll.sh`. A broadcast wakes nobody.

## What travels with it

The same registry that routes messages carries each session's work state
(working, blocked, waiting, done, idle), which is what colours the rows in
Sessions mode. See [Work-state colours](../concepts/work-state.md) and, for the
wire format and the registry layout, [The comm relay](../concepts/comm.md) and
the [Comm protocol](../ref/comm.md) reference.
