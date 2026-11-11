# The project chat

The Chat drawer (`Ctrl+Shift+K`) shows the messages of one Slack channel: the channel the selected row's folder maps to.
The records the window holds are listed oldest first, and the newest sits at the bottom. The view follows the newest, so
a message that arrives takes the bottom line and lifts the rest. An empty channel shows `no messages`.

ArrowUp and ArrowDown (`chat.mark_older`, `chat.mark_newer`) mark one message, starting at the newest, and the view moves to show it. The mark is a lighter band across the message with a blue bar at its left edge; it changes nothing else about the message. A scroll that leaves the mark out of the drawer moves it to the nearest message still in view.

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
Switching back to one of the last two channels shown draws the messages the drawer held for it as soon as the hub answers
for the row, with `reading…` in the title until it is read again. The read returns the channel's newest messages, which
replace the newest held; the older ones kept stay as read, so an edit or delete of one, or a change to a thread's reply count, made while the channel was away does
not reach the drawer for as long as the channel is one of the last two. The drawer keeps those messages in memory when the
Chat drawer is hidden.

The drawer is dark, as Slack's desktop app is in dark mode: light gray text on a near-black ground. Each group of messages starts
with a header line: the author's name in bold, a small gray badge for a post that came from a bot (`APP`), the drawer
(`DRAWER`) or a row (`ROW`), and the time in small gray text, on a 12-hour clock (`2:14 PM`, whatever the day). The badge comes from where Slack says the post came from, never from the
text, so a name or message that reads `[drawer]` or `ROW` shows no badge. The text follows as typed.

The first message of each local day, and the oldest message held, has a day divider above its header: the day's name
(`Today`, `Yesterday`, or `Monday, November 9th`, with the year after it for another year) centered in a small outlined box with a
thin line running out to each side. The divider is not text: a press or a drag never selects it, and a copy never includes it.
The day is your local day, found with one fixed offset from UTC for every message, so near a daylight saving change `Today` or
`Yesterday` can begin an hour from where your clock puts it.

A message that has replies has a line in Slack's style: `2 replies` (`1 reply`) in blue and, when the daemon knows it, the
last reply in gray (`Last reply today at 2:44 PM`, `yesterday at …`, `Nov 9 at …`, with the year for another year).

Messages from one author in a row share one header, as in Slack: a message with the same author and origin as the one before
it, on the same local day and less than five minutes after it, has no header of its own and its text follows the last one's. The
first message of each group has a small colored square in the margin at the left, a color chosen from eight by the author (so two authors can
share one); the text of every message starts right of that margin. Mark a message that has no header and its time (`2:01`, without AM or PM) shows in the margin beside it.
The five minutes are Slack's rule as remembered, and have not been checked against the desktop app. A page of older messages
that arrives above the top message can take that message's header and day divider off, which moves that message and every message
below it up by their height. The reverse also happens: the oldest message held always shows a header and a divider, so when the
oldest messages go (past 276), the one that becomes the oldest gains both, and the messages below it move down.

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
blue and is not rendered: a link is not clickable and a mention is not replaced by a name. Text that looks like
a header or a badge inside a message stays body text.

## Deleted messages

A message deleted while the drawer watches stays in its place, with its author, time and origin, and reads
`(message deleted)`. It counts toward the 256 messages the drawer keeps. A new snapshot (the window reads one after any
loss of the chat feed) drops it, since the snapshot holds only what Slack still has, except a deleted message older than the
snapshot's oldest when the snapshot reaches back to what the drawer holds, which stays; a snapshot that does not drops it too.

## When Slack is unreachable

The title does not show the state of the Slack connection. A socket outage shows only when it costs a snapshot, as
`not up to date`. The drawer keeps the records it holds until a new snapshot replaces them. `no messages` appears only
for a channel that was read whole and holds none.

## When the window's link drops

The records the drawer holds stay on screen and the title says `not connected`. When the link to the computer comes back,
the window watches the channel again and reads a new snapshot, which replaces the records; a lost chat feed on a link that
stays up does the same. A draft is kept through both.

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
`[chat] name` of `settings.toml` (see [the settings reference](../ref/config.md)); with no name set, nothing is sent: the foot of the drawer becomes the prompt `Name for your chat posts:` and the keys type the name
instead of the draft. `Enter` saves it to `settings.toml` (the file the window loaded, or the per-user one, made if missing) and uses it at
once, for every row and project in the window, and the draft stays: the next `Enter` posts it. `Escape` cancels and keeps the draft. A name
is refused, with the reason on the status line and the prompt open, when it is empty, holds a quote or a backslash, or is longer than
160 bytes (the prompt takes no more at 160 bytes), and a paste that holds a newline or another control character is refused whole, never
joined. The prompt ends, with the draft kept, when you change the row, hide the drawer, the link to the hub changes, or the row has no
mapped channel; scrolling, the mark and a thread's opening still work while it is open. A settings file the window will not edit safely (one that is not valid TOML, or spells `[chat]` or `name` in some other way) is left as it is, and the status line says how to add the name by hand. The name is asked once per settings file; to change it later, edit `[chat] name` and restart the window. While the post waits the draft is frozen: a key or `Enter` is refused with `posting…` on the status line. The draft is cleared only by a
reply that proves the post was made. Any other outcome keeps it, and the status line says why: `not posted` with the
reason (a `bad request` names `[chat] name`), a wait when Slack is rate limiting (`Enter` is refused until it passes), or
`may have been posted` when the answer was lost, in which case look at the channel before sending it again: a post is never
sent a second time on its own. A typed `<` posts as itself, and a post cannot mention or ping anyone: `@here` and
`<!channel>` arrive as plain text. A window posts only to a hub that is updated to escape text; to an older hub it says
`the hub must be updated to post` and keeps the draft.

## Selecting

Press the left mouse button on a message's text and drag to select within that message: the selection stays in the message
where you began, and the name and time line is not selectable. A click on the name and time line, in the margin, or anywhere else, selects
nothing, and a click in the messages still focuses the drawer. Any other click clears the selection, and so does a change
to the message (an edit or a delete), a new window width, opening a thread, and changing the row; moving the mark does not. A chat
selection is never shared with the agent; only the preview's text and your drawing are.

## Copying

`Ctrl+C` or `Ctrl+Shift+C` (`Cmd+C` on macOS; `chat.copy_selection`) with the Chat drawer focused copies the selected text
exactly as shown: Slack's markup as the markup text, `&lt;` and `&gt;` as `<` and `>`, and a deleted message as
`(message deleted)`, with the message's own line breaks and none from the wrap. With no selection it copies the marked
message's text, not its name and time line (a message without a header copies its whole text). With neither, the status line says there is nothing to copy. Ctrl+C in the
agent pane, the Terminal and the Julia REPL is unchanged.

## Moving the hub

The chat listens on one computer for the whole comm folder: the daemon that holds the lock in `chat/` of the comm folder
(`~/.sot-comm/chat/` by default). To move the hub, edit `hosts.toml` and start the new hub's daemon; nothing else. The old
hub's chat notices the new topology within a minute and stops listening, and a new hub whose start finds the lock still
held tries again each minute and listens once it is free. A computer that was lost should free its lock when the file server's
lease runs out, in about two minutes; that rests on the lease and has not been measured. A computer that becomes the hub starts its chat when its daemon next starts.
