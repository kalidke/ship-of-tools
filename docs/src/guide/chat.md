# The project chat

The Chat drawer (`Ctrl+Shift+K`) shows the messages of one Slack channel: the channel the selected row's folder maps to.
The records the window holds are listed oldest first, and the newest sits at the bottom. The view follows the newest, so
a message that arrives takes the bottom line and lifts the rest. An empty channel shows `no messages`.

To read older messages, scroll up: PageUp and PageDown move a third of the drawer, Alt+Up, Alt+Down and the mouse wheel one
line. A reader scrolled up stays where they are: a message that arrives, is edited or is deleted does not move the view,
and a message deleted at the reader's place stays there as `(message deleted)`; the next one takes the place only when a new snapshot or the trim drops it. `End` (`chat.latest`) returns to the newest and
follows it again, and scrolling down to the newest does the same. Two cases move the view: once the drawer holds its
100 newest messages, each arrival drops the oldest, so a reader on the oldest message moves up one message per arrival
(until older pages can be read); and deleting or shortening the newest message can bring the newest wholly into view,
which returns the reader to following. Switching to another row's channel starts at its newest.

Each message starts with its own header line: the author's name, a tag for a post that came from the drawer, a row or a
bot, and the time (`HH:MM` for today, with the date for an earlier day). The text follows as typed.

## Which channel

The hub's `chat.toml` maps a folder to a channel; a row whose folder is mapped shows that channel. How the mapping is
written is in [the chat configuration](../ref/config.md#chat.toml-and-chat-secrets.toml). The drawer follows the selected
row. Its title is `chat · <channel id>` (the id shows only while a channel is watched) followed by what the drawer is waiting for or why it shows nothing:

| the title says | meaning | asked again |
|---|---|---|
| `waiting for this row's listing` | the window has not listed the selected row yet | every frame |
| `not connected` | the row's computer is not connected | when it connects |
| `reading…` | the hub is being asked, or the channel is being read | |
| (nothing after the channel) | the channel was read whole | |
| `the row's folder maps to no channel` | the folder is not in `chat.toml` | at a link change or another row |
| `the hub has no usable chat configuration` | `chat.toml` is missing or invalid; the hub's daemon log says why | at a link change |
| `the chat runs on the hub, not on <host>` | the row's computer is not the hub | at a link change |
| `the row is gone or was replaced` | the row's id changed | when the listing gives the row a new id, or at a link change |
| `the hub could not answer: <reason>` | the hub refused for another reason | after a minute |
| `this daemon has no chat: update it` | the row's daemon is too old | at a link change |
| `the hub's chat service is not running` | the hub's log says why | each minute |
| `not up to date: <reason>` | the channel could not be read whole (`no snapshot` when the hub never answered) | each minute |
| `the row is gone or was replaced`, `the row now maps to another channel` or `the row's folder maps to no channel`, after the channel | the hub refused the watch for good | at a row or link change |

## Markup shows as markup

A message is shown as its poster typed it. Slack's markup, such as a mention or a link, is shown as the markup text in
its own color and is not rendered: a link is not clickable and a mention is not replaced by a name. Text that looks like
a header inside a message stays body text.

## Deleted messages

A message deleted while the drawer watches stays in its place, with its author, time and origin, and reads
`(message deleted)`. It counts toward the 100 messages the drawer keeps. A new snapshot (the window reads one after any
loss of the chat feed) drops it, since the snapshot holds only what Slack still has.

## When Slack is unreachable

The title does not show the state of the Slack connection. A socket outage shows only when it costs a snapshot, as
`not up to date`. The drawer keeps the records it holds until a new snapshot replaces them. `no messages` appears only
for a channel that was read whole and holds none.

## The draft

With the drawer focused, typing writes a draft at its foot: `> ` and the last three rows, then `to <channel>` and, if the
draft is longer, how many rows are not shown (a line wider than the drawer wraps, and each wrapped row counts). `Shift+Enter`
(`chat.newline`) starts another line, and a paste is added whole. There is one draft per row, and the channel it goes to
is the one shown when its first character was typed, even if you move the view afterwards. A draft stops at what a post
can carry (8 KiB, counting `&`, `<` and `>` as five, four and four bytes); a key or paste past it is refused whole and
the status line says so. Control characters other than tab are dropped. Nothing posts yet: `Enter` does nothing, and a
draft is lost when its row goes or the window closes.

## Not yet

Posting from the drawer, older pages of history, threads and sharing a selection with the agent are not built yet. Each
arrives with its own change, and this page says so when it does.
