# The project chat

The Chat panel shows the Slack channel of the selected project's session and posts to it. To connect Slack or to give a
project its channel, see [Setting up Slack](chat-setup.md).

Keys are written as on Linux and Windows; on a Mac, press Cmd where this page says Ctrl. A letter in a key is always
lowercase: `Ctrl+k` is Ctrl and k, and `Ctrl+Shift+k` adds Shift. Keys can be changed; see
[Keybindings](../ref/keybindings.md).

## Opening the panel

`Ctrl+k` opens the Chat panel in the bottom drawer and gives it the keys; `Ctrl+k` again hides it. The panel follows the
selected session: select another session and it shows that project's channel, starting at its newest message. A click
in the messages gives the panel the keys; Escape gives them back to the navigation tree (after closing an open thread
first).

The panel's top line reads `chat ·` and the channel (`chat · #lab-general`). When more than one Slack workspace is
connected it names the workspace first (`chat · Alpha Lab · #lab-general`). After the channel it says what the panel is
waiting for or why it shows nothing; [What the top line says](#What-the-top-line-says) lists every case. Messages about
what a key did appear on the status line above the navigation tree and stay there 10 seconds, unless a newer one
replaces them; [What the status line says](#What-the-status-line-says) lists them.

## Keys in the Chat panel

| Key | What it does |
|-----|--------------|
| `Ctrl+k` | Show or hide the Chat panel. |
| typing, `Ctrl+v` | Write the message at the panel's foot; a paste is added whole. |
| `Shift+Enter` | Start a new line in the message. |
| `Enter` | Post the message. With nothing typed in a project that has no channel, open the channel list. |
| `Ctrl+u` | Post the file the preview pane shows, with the message as its comment. |
| `ArrowUp`, `ArrowDown` | Mark a message: the first `ArrowUp` marks the newest, then each press marks the next older or newer one. |
| `ArrowRight` | Open the marked message's thread. |
| `ArrowLeft` | Close the open thread, while nothing is typed. |
| `Escape` | Close the open thread; with none open, give the keys back to the navigation tree. |
| `PageUp`, `PageDown` | Scroll a third of the panel. |
| `Alt+ArrowUp`, `Alt+ArrowDown`, mouse wheel | Scroll one line. |
| `End` | Go back to the newest message and follow new ones. |
| `Ctrl+c` | Copy the selected text, or the marked message's text when nothing is selected. |
| `Ctrl+o` | Open the marked message's first picture in the preview pane. A click on a picture opens that one. |
| `Ctrl+Shift+k` | Open the channel list: change this project's channel, pick one in another workspace, or connect a workspace. |

While the channel list is open, Up and Down choose, Left and Right show another workspace's channels, typing narrows
the list, Enter picks and Escape closes it.

## Reading

The newest message is at the bottom, and the panel follows it: a message that arrives takes the bottom line and lifts
the rest. Scroll up to read older messages; while you are scrolled up, the view stays where it is when messages arrive,
are edited or are deleted. `End`, or scrolling down to the newest, follows new messages again. A scroll glides to its
place over `smooth_scroll_ms` in the `[display]` table of `settings.toml` (150 ms unless you change it; 0 moves at once).

Scrolling to the oldest message reads older ones from Slack, up to 256 messages in all; the top line says when no more
can be read. An empty channel shows `no messages`.

The mark is a lighter band across the message with a blue bar at its left edge. The view moves to keep the marked
message in view, and a scroll that leaves it out of view moves the mark to the nearest message still shown.

Only the newest 100 messages follow edits and deletes made in Slack; an older one stays as it was read. A message
deleted while you watch stays in its place as `(message deleted)`; a later read of the channel usually removes it. The
panel keeps the messages of the last two channels it showed, so switching back draws them as soon as the computer that
runs your sessions answers, with `reading…` in the top line until the channel is read again.

## Threads

Mark a message with `ArrowUp` and press `ArrowRight`: the panel shows the message and its replies, oldest first, and
the top line adds `· thread`. Any message opens this way, with replies or without. While a thread is open, what you
write is a reply in it. `Escape` closes the thread, and so does `ArrowLeft` while nothing is typed; the mark is back on
the message. Selecting another session or hiding the panel closes the thread too.

A thread is read when it is opened, and again after your own reply in it: a reply someone makes elsewhere while it is
open shows when you open the thread again. A thread shows up to 156 messages; the top line says
`the rest of this thread is not shown` past that.

A message that has replies ends with a line in Slack's style: `2 replies` (`1 reply`) and, when it is known,
`Last reply today at 2:44 PM`.

## Writing and posting

### The foot of the panel

While the message box is empty, the foot of the panel shows one row:

`> type a message and press Enter to post it`

Under it, until the first post from this computer is made (from any project), a short explanation follows:

`What you post appears in the channel as your own message, from your Slack account. Ctrl+Shift+k changes this project's channel.`

After that first post the explanation is gone for good on this computer; the one-row prompt stays whenever the box is
empty. With an older Ship of Tools on the computer that runs your sessions, a warning that it posts as your Slack app
stays there instead. While a thread is open, the foot shows `> ` and `reply in thread of` the message's time
(`reply in thread of 2:14 PM`) instead of the prompt.

As you type, the foot shows `> ` and the last three rows of the message, and a row saying how many rows are not shown
when it is longer. Each project has its own message, which keeps the place it was begun in (the channel, or the thread
open then) through closing the thread, selecting another session and a reconnect; the foot names that place when the
top line does not. A message holds about 8 KiB; a key or paste past that is refused whole. It is lost when its session
ends or the window closes.

### Your chat name

If no chat name is set, the first time you press Enter with a message typed, the foot asks
`Your name for chat posts (saved in settings.toml):`. Type a name and press Enter: it is saved in `[chat] name` of
`settings.toml` (see [the settings reference](../ref/config.md)), and your message stays. Press Enter again to post it.
Escape cancels and keeps the message. A name with a quote, a backslash or a control character, or longer than 160
bytes, is refused. Your posts appear in Slack under your own Slack name; the chat name shows only when the computer that
runs your sessions runs an older Ship of Tools, which posts as your Slack app. To change the name, edit `[chat] name`
and restart the window.

### What happens when you post

`Enter` posts the message once, to the channel or thread the foot names, as your own message from your Slack account.
You must be a member of the channel in Slack. While the post waits, the message is frozen: a key or Enter is refused
with `posting…` on the status line. Only Slack's answer that the post was made clears the message; any other outcome
keeps it, and the status line says why. If the answer was lost (`may have been posted`), look at the channel before
posting again: a post is never sent twice on its own.

A post is plain text: what you type, a `<` included, arrives as typed, and `@here` and `<!channel>` arrive as plain
text that pings no one.

If the computer that runs your sessions holds no Slack code that posts as you (it was connected before Ship of Tools
posted as you, or Slack stopped accepting the code), nothing is posted, and the panel shows the `post as you` screen in
place of the channel, with your message kept: see
[Give a connected workspace new Slack codes](chat-setup.md#Give-a-connected-workspace-new-Slack-codes).

## Posting a file

`Ctrl+u` posts the file the preview pane shows, once, as your own message, with what you typed as its comment. It goes
to the channel or thread your message was begun in, or, with nothing typed, to the channel and the thread open then.
The status line says `shared <name> to <channel>` when it is posted, and the message is cleared.

The file must be one of the project's files, shown in the preview from the files tree: a file of the selected
session's folder, at most 16 MiB, and not one of the chat's own configuration files. A picture opened from the chat is
not a file of the project, so with one in the preview nothing is posted and the status line says so for 10 seconds:
`not posted: the preview shows a picture opened from the chat, which is not a file of this project; show a file in the
preview, then press Ctrl+u`. Select the file in the files tree, click in the Chat panel and press `Ctrl+u` again.
Nothing is posted either while the channel list or the name prompt is open.

A person's post of a file has not yet been checked against Slack itself.

## Pictures

A message's pictures (PNG, JPEG, GIF and WebP) show below its text as square tiles, each with Slack's thumbnail once
it arrives; any other file shows as a line with its name. A click on a tile (pressed and released in place) opens that
picture in the preview pane, and `Ctrl+o` opens the marked message's first picture. The status line says
`opening the picture…`, then `picture shown in the preview`, or why it was not opened.

A picture opened from the chat is not a project file: it has no name in the files tree, `Ctrl+u` does not post it, and
drawing, cropping and the scale bar do not work on it. Moving the cursor in the files tree, or selecting another
session, shows a file again. A very large picture is shown at reduced resolution.

## Selecting and copying

Press the left mouse button on a message's text and drag to select within that message; the name and time line is not
selected. `Ctrl+c` (or `Ctrl+Shift+c`) copies the selection as shown: a link's label, a mention's name, a style's text
without its marks. With nothing selected it copies the marked message's text. `(edited)` is never copied. The status
line says `copied chat text`, or `nothing to copy: select text or mark a message`. A chat selection is never shared
with the agent; only the preview's text and your drawing are.

## How messages look

The panel is dark, like Slack's desktop app in dark mode. Each message starts with a header: the author's name in bold,
a small gray badge for a post from a Slack app (`APP`) or from an agent in one of your sessions (`AGENT`), and the time
on a 12-hour clock (`2:14 PM`). Your own posts carry no badge. Messages from one author less than five minutes apart on
the same day share one header, as in Slack, with a small colored square in the margin; mark such a message to see its
time in the margin. A divider names each day (`Today`, `Yesterday`, `Monday, November 9th`). A message Slack marks as
edited ends with `(edited)`. The five-minute grouping is Slack's rule as remembered, and has not been compared with the
desktop app.

Slack's styles show as styles: `*bold*`, `_italic_`, `~strike~`, `` `code` ``, fenced code blocks and `>` quotes.
Links show their label (a link is not clickable), people as `@` and their name, channels as `#` and their name, and
`@here`, `@channel` and `@everyone` by name. A name not looked up yet shows as its label or its ID until the channel is
read again. Any other markup shows as typed, in blue. These rules are the panel's reading of Slack's, and have not yet
been compared with the desktop app side by side.

## The agent in the channel

### A reply from the agent

A session's agent posts to its project's channel with `sot-fe chat post [--thread <ts>] (<text> | --stdin)`, run in the
session. The post appears under the session's handle (its name for [messages between sessions](messaging.md)) as
`<handle> (row via Ship of Tools)`, in the thread named by
`--thread`, or else as a new message. It is sent once: it prints `posted <ts>` and exits 0; exits 2 with `refused` and a
reason, or `limited, wait <n> s`, when nothing was posted; and exits 3 with `unknown` or `no answer` when the post may
exist, in which case the agent does not post it again and reads the thread first. A reply is public to every member of
the channel. It is not mail: it does not come back to the session.

### A figure from the agent

Ask the session's agent to post a figure, and it runs `sot-fe chat post [--thread <ts>] --file <path> [<comment> |
--stdin]` in the session. The file must be in the session's folder: a relative path is taken from the folder the
command runs in, and a path outside the folder or with a `..` in it is refused before anything is sent. A link inside
the folder is followed, so a `data/results` link to another disk can be posted; the chat's own configuration files are
refused. Any regular file of at most 16 MiB can be posted; Slack shows it under `<handle> (row via Ship of Tools)`,
then the comment. It prints `posted <ts>`, or `posted file <id>` when Slack names no message, with the same exit codes
as a reply. Sharing into a thread with `--thread` has not yet been checked against Slack itself.

## Mail to the row

When `chat.toml` on the computer that runs your sessions sets `mail_after` (see
[the chat configuration](../ref/config.md#chat.toml-and-chat-secrets.toml)), each new post a channel member makes in a
mapped channel, yours from the panel included, is filed as one marked line of mail to the session mapped to that
channel's folder, with no window open. An idle Claude session is woken by it, a busy one reads it at the end of its
turn, and a Codex session gets it without being woken.

A post made while that computer was down or its Slack connection was lost, and a post whose filing was refused (the
session had not been seen for ten minutes, say), are found by a later recovery pass (one runs at least every 6 hours)
and filed once; the log of Ship of Tools' service on that computer shows each filing and each pass. A restart files nothing twice. Slack hides older
history on some plans, and a pass reads what Slack gives. Slack hands each new message to one connection of an app, so
any other program that connects with the chat app's App-Level Token takes some posts from it, and those wait for a
later pass.

## What the top line says

After the channel, the top line says what the panel is waiting for or why it shows nothing. `<computer>` is
`this computer`, the other computer's name, or `the computer that runs your agents`.

| The top line says | What it means | What to do |
|---|---|---|
| (nothing after the channel) | The channel was read whole. | |
| `reading…` | The channel is being read. | Wait. |
| `no channel` | The project has no channel. | Press Enter, or `Ctrl+Shift+k`, to pick one ([Setting up Slack](chat-setup.md)). |
| `set up Slack`, then a step's name | Slack is not connected, its saved codes are refused, or its chat did not start. | Follow the screens ([Setting up Slack](chat-setup.md)). |
| `post as you` | Your post was refused: no Slack code that posts as you is saved. | Give the workspace new codes, keeping its app ([Setting up Slack](chat-setup.md#Give-a-connected-workspace-new-Slack-codes)). |
| `connecting…`, `connected` | The codes are being checked; they were accepted. | |
| `choose this project's channel` | The channel list is open. | Pick a channel, or press Escape. |
| `your Slack app cannot read this channel` | Slack says your app is not in the channel: a private channel it was never invited to, or a new app. | In that channel in Slack, type `/invite @` and the app's name; the panel says which name. |
| `not up to date: <reason>` | The channel could not be read whole (`no messages read yet` when nothing came). | It is asked again each minute. |
| `no connection to <computer>` | The window is not connected to that computer. The messages held stay on screen. | It reads the channel again when the link comes back; a message you typed is kept. |
| `waiting for <computer> to report this project` | The window has not yet listed the selected session. | Wait. |
| `the chat does not run on <computer>; ...` | The session runs on a computer other than the one named on the `hub =` line of `hosts.toml`. | Run the project's session on that computer. |
| `the chat on <computer> is not set up correctly: ...` | `chat.toml` or a secrets file was refused. | Run `sotd log-path` in a terminal on that computer; the log names the file and line. |
| `the chat is not running on <computer>: ...` | The chat stopped there. | The log on that computer says why. |
| `the chat on <computer> could not answer: <reason>` | It refused for another reason. | It is asked again after a minute. |
| `Ship of Tools on <computer> has no chat: update it` | That computer runs an older Ship of Tools. | Update it. |
| `the row is gone or was replaced` | The session ended or was replaced. | Select the project's session again. |
| `the row now maps to another channel`, `the row's folder maps to no channel` | The channel changed while it was shown. | Select another session and back. |
| `your Slack plan hides older messages`, `older messages could not be read`, `older messages are not read: <reason>`, `older messages beyond the limit are not shown` | Scrolling to the oldest found no more to read. | |
| `the thread is not read: <reason>`, `the rest of this thread is not shown` | In a thread: a read was refused (it is asked again in a few seconds), or the thread is longer than 156 messages. | |

The channel shows as `#` and its name, or as its ID while its name is not known yet. The top line does not show
whether the computer that runs your sessions is connected to Slack right now; a lost connection shows only when the
channel cannot be read whole.

## What the status line says

| The status line says | What to do |
|---|---|
| `posting…` | Wait for the post's answer. |
| `nothing to post` | Type a message first. |
| `not posted: <reason>`, such as `you are not in that channel in Slack; join it there first` | Fix the reason; the message is kept. |
| `not posted: Slack asked for a pause; wait <n> s` | Wait, then press Enter again. |
| `may have been posted: check the channel before sending it again` | Look at the channel before posting again. |
| `not posted: update Ship of Tools on <computer>, then post again` | That computer runs an older Ship of Tools. |
| `not posted: close the screen open in the chat first` | Close the channel list or the screen shown, then post. |
| `not posted: the chat name is not valid; ...` | Fix the `name =` line under `[chat]` in `settings.toml`. |
| `draft is full: nothing added` | The message is at its limit. |
| `not posted: the preview shows no file` | Select a file in the files tree, then press `Ctrl+u`. |
| `not posted: the preview shows a picture opened from the chat, ...` | Select a file in the files tree, then press `Ctrl+u`. |
| `not posted: the file is not a readable file in the row's folder`, `not posted: the file is larger than the chat carries` | Post a file of the project, at most 16 MiB. |
| `shared <name> to <channel>` | The file was posted. |
| `chat name saved to <path>` | Your chat name is saved; press Enter to post. |
| `no message marked: ArrowUp marks one`, `the marked message has no picture` | Mark a message with a picture, then press `Ctrl+o`. |
| `opening the picture…`, `picture shown in the preview`, `picture not opened: <reason>` | The picture is on its way, shown, or refused. |
| `copied chat text`, `nothing to copy: select text or mark a message` | |
| `not set: <reason>` | The channel was not picked; the list stays open and the reason says what to do. |
| `that is not a Slack code: ...` | While connecting: copy the code named, by how it starts. |
| `could not open the browser (...): go to api.slack.com/apps` | Open that address yourself. |

## Moving the computer the chat runs on

The chat listens on one computer for all the computers that share the sessions' message folder (`~/.sot-comm` by
default): the one that holds the lock in its `chat/` folder. To move it, edit the `hub =` line of `hosts.toml` and
start Ship of Tools' service on the new computer; nothing else. The old computer's chat notices within a minute, finishes the one post it is filing, and stops;
the new one tries each minute and starts listening once the lock is free. A computer that was lost should free its lock
when the file server's lease runs out, in about two minutes; that has not been measured. Posts made in the minute
around a move can reach their session hours late, at the new computer's next recovery pass.
