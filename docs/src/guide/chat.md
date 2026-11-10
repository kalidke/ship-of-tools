# The project chat

The Chat drawer (`Ctrl+Shift+K`) shows the messages of one Slack channel: the channel the selected row's folder maps to.
The records the window holds are listed oldest first, and the newest sits at the bottom. The view follows the newest, so
a message that arrives takes the bottom line and lifts the rest. An empty channel shows `no messages`.

ArrowUp and ArrowDown (`chat.mark_older`, `chat.mark_newer`) mark one message, starting at the newest, and the view moves to show it; a scroll that leaves the mark out of the drawer moves it to the nearest message still in view.

ArrowRight (`chat.thread_open`) opens the marked message's thread, a message with no replies included: the thread shows the message and its replies, oldest first, and Escape closes it, with the mark back on the message. Changing the row or hiding the drawer closes it too. A thread is read when it is opened, not watched: a reply made on the phone shows when the thread is opened again, and `N replies` on a message is not live either. Each open takes about 8 Slack reads for a long thread, more when Slack sends short pages, plus the lookups of the authors' names. A thread shows up to 156 messages, the parent included: past that the newest replies are not shown, and the title says so. While a thread is open the channel's older messages give way to it, never its newest 100. A refused read is asked again after a few seconds (thirty when Slack rate limits), and the title says why. A message begun while a thread is open is a reply in that thread (the line under the draft reads `reply in thread of` the message's time), and one begun in the channel stays the channel's when a thread opens; a draft keeps where it was begun through closing the thread, changing the row and a reconnect. After your own reply is posted and the thread is open, it is read again, which costs about what opening it did (about 8 reads for a long thread), so it shows the parent alone for a moment.

To read older messages, scroll up: PageUp and PageDown move a third of the drawer, Alt+Up, Alt+Down and the mouse wheel one
line. A reader scrolled up stays where they are: a message that arrives, is edited or is deleted does not move the view,
and a message deleted at the reader's place stays there as `(message deleted)`; the next one takes the place only when a new snapshot or the trim drops it. `End` (`chat.latest`) returns to the newest and
follows it again, and scrolling down to the newest does the same. One case moves the view: deleting or shortening the newest message can bring the newest wholly into view,
which returns the reader to following. Scrolling to the oldest message reads older pages from Slack, up to the 256 messages the drawer keeps; the title says when a
refused or empty read stops it, when Slack's plan hides older messages and when the full drawer leaves older ones unshown. Messages
older than the newest 100 are as read: an edit or delete of one does not reach the drawer. The drawer drops the oldest past 256, never the message
the reader is on or a newer one, so a reader on the oldest message stays there while the drawer holds up to 276; past 276 the
oldest go anyway and the view moves to the next message that is kept. Switching to another row's channel starts at its newest.

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
`(message deleted)`. It counts toward the 256 messages the drawer keeps. A new snapshot (the window reads one after any
loss of the chat feed) drops it, since the snapshot holds only what Slack still has, except a deleted message older than the
snapshot's oldest when the snapshot reaches back to what the drawer holds, which stays; a snapshot that does not drops it too.

## When Slack is unreachable

The title does not show the state of the Slack connection. A socket outage shows only when it costs a snapshot, as
`not up to date`. The drawer keeps the records it holds until a new snapshot replaces them. `no messages` appears only
for a channel that was read whole and holds none.

## The draft

With the drawer focused, typing writes a draft at its foot: `> ` and the last three rows, then where it goes (`to <channel>`, or `reply in thread of` the parent's time for a draft begun in a thread, and for the next one while a thread is open) and, if the
draft is longer, how many rows are not shown (a line wider than the drawer wraps, and each wrapped row counts). `Shift+Enter`
(`chat.newline`) starts another line, and a paste is added whole. There is one draft per row, and where it goes
(the channel, or the thread open then) is fixed when its first character is typed, even if you move the view afterwards. A draft stops at what a post
can carry (8 KiB, counting `&`, `<` and `>` as five, four and four bytes); a key or paste past it is refused whole and
the status line says so. Control characters other than tab are dropped. A draft is lost when its row goes or the window
closes.

## Posting

`Enter` (`chat.send`) posts the draft once, to the channel or thread its compose line names. The post carries the name in
`[chat] name` of `settings.toml` (see [the settings reference](../ref/config.md)); with no name set, nothing is sent and the status line
says so. While the post waits the draft is frozen: a key or `Enter` is refused with `posting…` on the status line. The draft is cleared only by a
reply that proves the post was made. Any other outcome keeps it, and the status line says why: `not posted` with the
reason (a `bad request` names `[chat] name`), a wait when Slack is rate limiting (`Enter` is refused until it passes), or
`may have been posted` when the answer was lost, in which case look at the channel before sending it again: a post is never
sent a second time on its own. A typed `<` posts as itself, and a post cannot mention or ping anyone: `@here` and
`<!channel>` arrive as plain text. A window posts only to a hub that is updated to escape text; to an older hub it says
`the hub must be updated to post` and keeps the draft.

## Selecting

Press the left mouse button on a message's text and drag to select within that message: the selection stays in the message
where you began, and the name and time line is not selectable. A click on the name and time line, or anywhere else, selects
nothing, and a click in the messages still focuses the drawer. Any other click clears the selection, and so does a change
to the message (an edit or a delete), a new window width, moving the mark onto or off it, and changing the row. A chat
selection is never shared with the agent; only the preview's text and your drawing are.

## Copying

`Ctrl+C` or `Ctrl+Shift+C` (`Cmd+C` on macOS; `chat.copy_selection`) with the Chat drawer focused copies the selected text
exactly as shown: Slack's markup as the markup text, `&lt;` and `&gt;` as `<` and `>`, and a deleted message as
`(message deleted)`, with the message's own line breaks and none from the wrap. With no selection it copies the marked
message's text, not its name and time line. With neither, the status line says there is nothing to copy. Ctrl+C in the
agent pane, the Terminal and the Julia REPL is unchanged.
