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

When you clear a Claude Code or Codex row, or its context is compacted, the session stays joined. Its start
hook restores the handle, the handoff file if one exists, and any question still waiting for you. The question
stays open and the next poll continues from the same place. Codex also restores this context on resume.
A bare `/clear` is enough. These hooks apply to sessions launched with the updated integration.
In a live Claude Code session, `/resume` runs without the read-only hook.

## Ask, tell, broadcast

The agents call these for you when you say "ask the kernel session whether…",
but they are plain scripts in `~/.sot-comm/bin`:

```bash
comm-list.sh                                 # this computer's sessions and their state (a send reaches others too)
comm-send.sh @other-session "message"        # one result: filed, or FAILED with the reason
comm-send.sh --broadcast "message"           # filed to every live session, one result each
comm-send.sh @other-session --file notes.md  # the body is the file's bytes: quotes and $( ) arrive as written
comm-send.sh @other-session - < notes.md     # the body on stdin, the same way
comm-poll.sh                                 # read what arrived while you were away (16,000 bytes at a time; run it again when it says more remain)
```

Each script runs `sotd comm`. A send appends the message to the recipient's
inbox (`~/.sot-comm/inbox/<handle>.jsonl`), itself or through the Ship of Tools
daemon that owns that folder, and says `filed` only once the line is there; a
session that was busy or away reads it with `comm-poll.sh` when it gets to it.
A poll prints each message as a start line that begins with `[`, the time and the sender, followed by the message's own
text, and every later line of that text behind `> `, so a line that starts with `[` is always a message's own start and
no message's text can pass for another message.
If directed mail is waiting when a Codex turn tries to finish, its Stop hook asks the session to poll.
After the mail is read, the turn can finish. If a writer holds the inbox lock past the read timeout,
the hook lets the turn finish and checks again at the next turn end.

## A message wakes the recipient

An idle session does not have to be polling. A message that lands for a session
wakes it with one short fixed line, and the session reads its messages itself
with `comm-poll.sh`, so a burst of messages costs one wake (and one poll for each
16,000 bytes of it). A post from a mapped Slack channel (a line from `#chat`,
filed by the hub's daemon, see [the project chat](chat.md#Mail-to-the-session))
wakes a session in the same way, with the same one fixed line: none of the
post's words is typed into a row or put in a queue.

A Claude Code session at its prompt has the line typed into its row. That
typing is refused while the row shows a permission dialog or a menu, so it can
never answer one by accident, and while the prompt holds anything you have
typed and not sent, so it never touches your line.

A Codex session is never typed into. Its computer's daemon puts the same line
in Codex's own queue for the session's conversation, once for each batch of
unread mail, whatever the session's screen shows, and Codex hands it to the
session as a new request when the session is free. What is known of that comes
from trials on Codex 0.171.0, not yet from a session woken in use: a request
queued during a turn was taken after that turn, and a draft in the input stayed
there unsent. A request made while an approval question is open has not been
tried. A session at work is asked too, so mail that arrives during a turn can
cost one more turn: the session reads the mail as its turn ends, then takes the
queued line and polls again for nothing. The mail waits for the session's next
`comm-poll.sh` when the session has not yet said which conversation it is in
(it is asked once it has), when Codex's queue command is not there (it is tried
again no sooner than ten minutes later), or when the command fails or runs past
10 seconds (no second request is made for that batch). A request made to a
conversation you have just left waits in that conversation.

A Codex session whose agent started without Ship of Tools' Codex hooks never
says which conversation it is in, so mail to it starts no turn and waits for its
next `comm-poll.sh`, which comes only when something else starts a turn there.
That is every Codex session that was already running when its computer moved
from 0.7.3 or earlier to this version, and one started where the Codex hooks
are not installed or Codex does not accept `--no-daemon`. Sessions mode marks
such a session `[not woken by mail]`. Mail wakes it once its agent is started
again with the hooks.

## What travels with it

The same registry that routes messages carries each session's work state
(working, question, waiting, done, idle), which is what colours the rows in
Sessions mode. See [Work-state colours](../concepts/work-state.md) and, for the
wire format and the registry layout, [The comm relay](../concepts/comm.md) and
the [Comm protocol](../ref/comm.md) reference.
