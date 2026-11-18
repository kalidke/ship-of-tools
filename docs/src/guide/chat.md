# The project chat

The Chat panel, at the bottom of the Ship of Tools window, shows a project's Slack channel and posts to it. To connect
Slack or to give a project its channel, see [Setting up Slack](chat-setup.md).

Keys are written as on Linux and Windows. On a Mac, press Cmd where this page says Ctrl and Option where it says Alt
(`Ctrl+Shift+k` is Cmd+Shift+k); the window's own key lists show the Mac's symbols, such as `⇧⌘k` (⇧ is Shift, ⌘ is
Cmd). A letter in a key is always lowercase: `Ctrl+k` is Ctrl and k, and `Ctrl+Shift+k` adds Shift. Keys can be
changed; see [Keybindings](../ref/keybindings.md).

## What the panel shows

- **A session's channel.** A session is one coding agent, such as Claude Code or Codex, running in one project's
  folder. The navigation tree, the window's left column, has modes: `s` there shows Sessions mode, which lists the
  sessions, and `f` Files mode, which lists the project's files. The panel shows the channel of the selected session's
  project, and follows the selection. A message that says `row`, such as `the row is gone or was replaced` or
  `<handle> (row via Ship of Tools)`, means a session.
- **Where it is.** The bottom of the window shows one thing at a time: the Julia REPL, a terminal, the monitor, Help or
  the Chat panel. `Ctrl+k` shows the Chat panel there and gives it the keys; `Ctrl+k` again hides it. A click in the
  messages gives it the keys too, and Escape gives them back to the navigation tree, after closing an open thread first.
- **Its top line.** The panel's first line reads `chat ·` and the channel (`chat · #lab-general`); when more than one
  Slack workspace is connected it names the workspace first (`chat · Alpha Lab · #lab-general`). After the channel it
  says what the panel is waiting for or why it shows nothing; [What the top line says](#What-the-top-line-says) lists
  every case.
- **The status line.** What a key did is said on the status line, the line starting `status:` at the top of the
  navigation tree, for 10 seconds or until a newer message replaces it;
  [What the status line says](#What-the-status-line-says) lists the messages.
- **The computer that runs your sessions.** On each computer, Ship of Tools' service (`sotd`, a background program apart
  from the window) runs the sessions there. The chat runs on one computer, which this page calls the computer that runs
  your sessions: with one computer, that one; with several, the one your `hosts.toml` (the file that lists your
  computers, in `~/.config/sot` on Linux and macOS and `%LOCALAPPDATA%\sot\config` on Windows) names in its line
  `hub = "<computer>"`. The panel's messages name a computer by its name, or `this computer` for the one the window runs
  on; where a message says `the computer that runs your agents`, it means the computer that runs your sessions.
- **Your Slack app, your account and the Slack codes.** What you write goes out under your own Slack account. What your
  sessions' agents write goes out through your Slack app, the app you made in Slack while setting up, which also reads
  the channel. Three Slack codes (Slack calls them tokens), pasted during setup and kept on the computer that runs your
  sessions, let Ship of Tools act in Slack: one posts as you, one lets your Slack app read and post, and one lets it
  receive new messages.

## Keys in the Chat panel

| Key | What it does |
|-----|--------------|
| `Ctrl+k` | Show or hide the Chat panel. |
| typing, `Ctrl+v` | Write the message at the panel's foot; a paste is added whole. |
| `Shift+Enter` | Start a new line in the message. |
| `Enter` | Post the message. With nothing typed in a project that has no channel, open the channel list. |
| `Ctrl+u` | Post the file the preview pane shows, with the message as its comment. |
| `ArrowUp`, `ArrowDown` | Mark a message (a highlight that the keys below act on): the first `ArrowUp` marks the newest, then each press marks the next older or newer one. |
| `ArrowRight` | Open the marked message's thread. |
| `ArrowLeft` | Close the open thread, while nothing is typed. |
| `Escape` | Close the open thread; with none open, give the keys back to the navigation tree. |
| `PageUp`, `PageDown` | Scroll a third of the panel. |
| `Alt+ArrowUp`, `Alt+ArrowDown`, mouse wheel | Scroll one line. |
| `End` | Go back to the newest message and follow new ones. |
| `Ctrl+c`, `Ctrl+Shift+c` | Copy the selected text, or the marked message's text when nothing is selected. |
| `Ctrl+o` | Open the marked message's first picture in the preview pane. A click on a picture opens that one. |
| `Ctrl+Shift+k` | Open the channel list: change this project's channel, pick one in another workspace, or connect a workspace. |

While the channel list is open, Up and Down choose, Left and Right show another workspace's channels, typing narrows
the list, Enter picks and Escape closes it.

## Reading

The newest message is at the bottom, and the panel follows it: a message that arrives takes the bottom line and lifts
the rest. Scroll up to read older messages; while you are scrolled up, the view stays where it is when messages arrive,
are edited or are deleted. `End`, or scrolling down to the newest, follows new messages again. Selecting another session
shows its channel from the newest message. A scroll glides to its place over `smooth_scroll_ms`, a setting in the
`[display]` table of the window's settings file, `settings.toml` (in `~/.config/sot` on Linux and macOS and
`%LOCALAPPDATA%\sot\config` on Windows): 150 ms unless you change it, and 0 moves at once.

Scrolling to the oldest message reads older ones from Slack, up to 256 messages in all, a fixed limit of Ship of
Tools; past it the top line says `older messages beyond the limit are not shown`. An empty channel shows `no messages`.

The mark is a lighter band across the message with a blue bar at its left edge. The view moves to keep the marked
message in view, and a scroll that leaves it out of view moves the mark to the nearest message still shown.

Edits and deletes made in Slack reach the newest 100 messages, a fixed limit of Ship of Tools; an older message edited
in Slack after the panel read it keeps the text it had then. A message deleted while you watch stays in its place as `(message deleted)` until the panel reads the
channel again, as it does after a lost connection and when you come back to the channel from another session. The panel
keeps the messages of the last two channels it showed, so switching back draws them as soon as the computer that runs
your sessions answers, with `reading…` in the top line until the channel is read again.

## Threads

Mark a message with `ArrowUp` and press `ArrowRight`: the panel shows the message and its replies, oldest first, and
the top line adds `· thread`. Any message opens this way, with replies or without. While a thread is open, what you
write is a reply in it. `Escape` closes the thread, and so does `ArrowLeft` while nothing is typed; the mark is back on
the message. Selecting another session or hiding the panel closes the thread too.

A thread is read when it is opened, and again after your own reply in it: a reply someone makes elsewhere while it is
open shows when you open the thread again. A very long thread shows its oldest messages, and the top line says
`the rest of this thread is not shown`.

A message that has replies ends with a line in Slack's style: `2 replies` (`1 reply`) and, when Slack gives its time,
`Last reply today at 2:44 PM`.

## Writing and posting

### The foot of the panel

The foot of the panel is its last lines, under the messages. While the message box there is empty, it shows one line:

`> type a message and press Enter to post it`

Under it, until the first post from the computer the window runs on is made (from any project), a short explanation
follows:

`What you post appears in the channel as your own message, from your Slack account. Ctrl+Shift+k changes this project's channel.`

After that first post the explanation is gone for good on that computer; the one-line prompt stays whenever the box is
empty. While a thread is open, the foot shows `> ` and `reply in thread of` the message's time
(`reply in thread of 2:14 PM`) instead of the prompt.

As you type, the foot shows `> ` and the last three lines of the message, and a line such as `2 lines not shown` when
it is longer. Each project has its own message, which keeps the place it was begun in (the channel, or the thread
open then) through closing the thread, selecting another session and a reconnect; the foot names that place when the
top line does not. A message holds about 8 KiB, a fixed limit of Ship of Tools; a key or paste past that is refused
whole, with `draft is full: nothing added` (the draft is your message). It is lost when its session ends or the window closes.

### With an older Ship of Tools

An older Ship of Tools on the computer that runs your sessions posts what you write as your Slack app, under a chat
name, with no badge; the foot shows a warning saying so in place of the explanation. If no chat name is set, the first
time you press Enter with a message typed, the foot asks
`Your name for chat posts (saved in settings.toml):`. Type a name and press Enter: it is saved in `[chat] name` of
`settings.toml` (see [the settings reference](../ref/config.md)), and your message stays. Press Enter again to post it.
Escape cancels and keeps the message. A name with a quote, a backslash or a control character, or longer than 160
bytes, is refused. Otherwise your posts appear in Slack under your own Slack name, Enter posts with no question, and the
chat name is not used. To change the name, edit `[chat] name` and restart the window.

### What happens when you post

`Enter` posts the message once, to the channel or thread the foot names, as your own message from your Slack account.
You must be a member of the channel in Slack. While the post waits, the message is frozen: a key or Enter is refused
with `posting…` on the status line. Only Slack's answer that the post was made clears the message; any other outcome
keeps it, and the status line says why. If the answer was lost (`may have been posted`), look at the channel before
posting again: a post is never sent twice on its own.

A post is plain text: what you type, a `<` included, arrives as typed, and `@here` and `<!channel>` arrive as plain
text that pings no one.

If the computer that runs your sessions holds no Slack code that posts as you (it was connected before Ship of Tools
posted as you, or Slack stopped accepting the code), nothing is posted, and the panel shows the setup page's
post-as-you screen, which asks for that workspace's three Slack codes again, in place of the channel, with your message
kept: see
[Give a connected workspace new Slack codes](chat-setup.md#Give-a-connected-workspace-new-Slack-codes).

## Posting a file

`Ctrl+u` posts the file the preview pane shows, once, as your own message, with what you typed as its comment. The
preview pane is the window's middle column, which shows the file selected in the navigation tree's Files mode (press
`f` there). The file goes to the channel or thread your message was begun in, or, with nothing typed, to the channel and
the thread open then. The status line says `shared <name> to <channel>` when it is posted, and the message is cleared.

The file must be a file of the selected session's folder, at most 16 MiB (a fixed limit of Ship of Tools; a larger one
is refused with `not posted: the file is larger than the chat carries`), and not one of the chat's own settings files. A picture opened from the chat is not a file of the project, so with one in the preview
nothing is posted and the status line says so for 10 seconds:
`not posted: the preview shows a picture opened from the chat, which is not a file of this project; show a file in the
preview, then press Ctrl+u`. Select the file in Files mode, click in the Chat panel and press `Ctrl+u` again. Nothing is
posted either while the channel list or the name prompt is open.

## Pictures

A message's pictures (PNG, JPEG, GIF and WebP) show below its text as square tiles, each with Slack's thumbnail once
it arrives; any other file shows as a line with its name. A click on a tile (pressed and released in place) opens that
picture in the preview pane, and `Ctrl+o` opens the marked message's first picture. The status line says
`opening the picture…`, then `picture shown in the preview`, or why it was not opened.

A picture opened from the chat is not a project file: it has no name in Files mode, `Ctrl+u` does not post it, and
the preview's own tools (drawing over a picture for the agent, cropping it, its scale bar) do not work on it. Moving the cursor in Files mode, or selecting another session,
shows a file again. A very large picture is shown at reduced resolution.

## Selecting and copying

Press the left mouse button on a message's text and drag to select within that message; the name and time line is not
selected. `Ctrl+c` (or `Ctrl+Shift+c`) copies the selection as shown: a link's label, a mention's name, a style's text
without its marks. With nothing selected it copies the marked message's text. `(edited)` is never copied. The status
line says `copied chat text`, or `nothing to copy: select text or mark a message`. The session's agent never sees what
you select in the chat; it sees only text you select in the preview pane and what you draw there.

## How messages look

The panel is dark, like Slack's desktop app in dark mode. Each message starts with a header: the author's name in bold,
a small gray badge, and the time on a 12-hour clock (`2:14 PM`). The badge is `AGENT` for a post from one of your
sessions' agents (sent through your Slack app) and `APP` for any other post from a Slack app; your own posts carry
none. Messages from one author less than
five minutes apart on the same day share one header, with a small colored square in the margin; mark such a message to
see its time in the margin. A divider names each day (`Today`, `Yesterday`, `Monday, November 9th`). A message Slack
marks as edited ends with `(edited)`.

Slack's styles show as styles: `*bold*`, `_italic_`, `~strike~`, `` `code` ``, fenced code blocks and `>` quotes.
Links show their label (a link is not clickable), people as `@` and their name, channels as `#` and their name, and
`@here`, `@channel` and `@everyone` by name. A name not looked up yet shows as its label or its ID until the channel is
read again. Any other markup shows as typed, in blue.

## The agent in the channel

A session's agent can post to its project's channel with the command `sot-fe chat post`, which Ship of Tools installs
for agents to run. Its posts appear as `<handle> (row via Ship of Tools)`, where the handle is the session's name as
Sessions mode shows it, the name it also signs its messages to other sessions with
([Agents messaging each other](messaging.md)). In the commands below, `<ts>` is a message's Slack timestamp, which
names it.

### A reply from the agent

The agent runs `sot-fe chat post [--thread <ts>] (<text> | --stdin)` in the session. The post goes into the thread named
by `--thread`, or else is a new message. It is sent once: the command prints `posted <ts>` and exits 0; exits 2 with
`refused` and a reason, or `limited, wait <n> s`, when nothing was posted; and exits 3 with `unknown` or `no answer`
when the post may exist, in which case the agent does not post it again and reads the thread first. A reply is public to
every member of the channel.

### A figure from the agent

Ask the session's agent to post a figure, and it runs `sot-fe chat post [--thread <ts>] --file <path> [<comment> |
--stdin]` in the session. The file must be in the session's folder: a relative path is taken from the folder the
command runs in, and a path outside the folder or with a `..` in it is refused before anything is sent. A link inside
the folder is followed, so a `data/results` link to another disk can be posted; the chat's own settings files are
refused. Any regular file of at most 16 MiB (a fixed limit of Ship of Tools; a larger one is refused, exit 2) can be
posted; Slack shows it under
`<handle> (row via Ship of Tools)`, then the comment. With `--thread` it is posted into that thread; if it shows in the
channel instead, Slack did not take the thread. The command prints `posted <ts>`, or `posted file <id>` when Slack names
no message, with the same exit codes as a reply.

## Mail to the session

Posts in a project's channel can reach the session's agent as mail (the messages sessions receive from each other), so
the agent can answer in the channel with no window open.

*The rest of this section is for whoever administers the computer that runs your sessions.*

Mail is off until `mail_after`, the time from which posts count as mail (a Slack timestamp, `<seconds>.<six digits>`),
is set in `chat.toml`, the chat's settings file; [the chat configuration](../ref/config.md#chat.toml-and-chat-secrets.toml)
says how. Then each new post a channel member makes in a mapped channel (one picked for a project), yours from the
panel included, reaches that project's session as a message to its agent. An idle Claude Code agent starts on it at
once, a busy one reads it when it finishes what it is doing, and a Codex agent reads it the next time it checks its
messages. An agent's own reply is not mail and does not come
back to it. A post missed while the computer was down is filed later, once; the configuration page says how.

## What the top line says

After the channel, the top line says what the panel is waiting for or why it shows nothing. `<computer>` is a
computer's name, `this computer`, or `the computer that runs your agents`.

| The top line says | What it means | What to do |
|---|---|---|
| (nothing after the channel) | The channel was read whole. | |
| `reading…` | The channel is being read. | Wait. |
| `no channel` | The project has no channel. | Press Enter, or `Ctrl+Shift+k`, to pick one ([Setting up Slack](chat-setup.md)). |
| `set up Slack`, then a step's name | Slack is not connected, its saved codes are refused, or its chat did not start. | Follow the screens ([Setting up Slack](chat-setup.md)). |
| `post as you` | Your post was refused: no Slack code that posts as you is saved. | Give the workspace new codes, keeping its Slack app ([Setting up Slack](chat-setup.md#Give-a-connected-workspace-new-Slack-codes)). |
| `connecting…`, `connected` | The setup's codes are being checked; they were accepted. | |
| `a different Slack app` | The codes pasted belong to another Slack app than the one the workspace is connected to now; nothing was changed. | Press Enter to keep the app connected now, or Right to connect the new one ([Setting up Slack](chat-setup.md#If-you-paste-the-codes-of-a-new-app)). |
| `choose this project's channel` | The channel list is open. | Pick a channel, or press Escape. |
| `your Slack app cannot read this channel` | Slack says your Slack app is not in the channel: a private channel it was never invited to, or a new app. | In that channel in Slack, type `/invite @` and the app's name; the panel says which name. |
| `not up to date: <reason>` | The channel could not be read whole (`no messages read yet` when nothing came). | Wait: the panel tries again each minute. If it stays for several minutes, the log on that computer says why (`sotd log-path`, below). |
| `no connection to <computer>` | The window is not connected to that computer. The messages held stay on screen. | Wait: the panel reads the channel again when the link comes back, and a message you typed is kept. |
| `waiting for <computer> to report this project` | The window has not yet listed the selected session. | Wait. |
| `the chat does not run on <computer>; ...` | The project's session runs on another computer than the one the chat runs on, which `hosts.toml` names on its `hub =` line. | Run the project's session on the computer the chat runs on. |
| `the chat on <computer> is not set up correctly: ...` | `chat.toml` or a file of Slack codes was refused. | Run `sotd log-path` in a terminal on that computer: it prints where the log of Ship of Tools' service is, and the log names the file and line. |
| `the chat is not running on <computer>: ...` | The chat stopped there. | The log on that computer says why. |
| `the chat on <computer> could not answer: <reason>` | It refused for another reason. | Wait: the panel tries again each minute. |
| `Ship of Tools on <computer> has no chat: update it` | That computer runs an older Ship of Tools. | Update it. |
| `the row is gone or was replaced` | The session ended or was replaced. | Select the project's session again. |
| `the row now maps to another channel`, `the row's folder maps to no channel` | The channel picked for the project changed, or was removed, while it was shown. | Select another session, then this one again: the panel asks afresh. |
| `your Slack plan hides older messages`, `older messages could not be read`, `older messages are not read: <reason>`, `older messages beyond the limit are not shown` | Scrolling to the oldest found no more to read: Slack keeps older history only on some plans, a read was refused, or the panel's 256 are full. | |
| `the thread is not read: <reason>`, `the rest of this thread is not shown` | In a thread: a read was refused (the panel tries again by itself), or the thread is longer than the panel shows. | |

The channel shows as `#` and its name, or as its ID while its name is not known yet. The top line does not say whether
Slack itself can be reached from the computer that runs your sessions; when that connection is lost long enough for
messages to be missed, the top line says `not up to date`.

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
| `not posted: the preview shows no file` | Select a file in Files mode, then press `Ctrl+u`. |
| `not posted: the preview shows a picture opened from the chat, ...` | Select a file in Files mode, then press `Ctrl+u`. |
| `not posted: the file is not a readable file in the row's folder`, `not posted: the file is larger than the chat carries` | Post a file of the project's folder, at most 16 MiB. |
| `shared <name> to <channel>` | The file was posted. |
| `chat name saved to <path>` | Your chat name is saved; press Enter to post. |
| `no message marked: ArrowUp marks one`, `the marked message has no picture` | Mark a message with a picture, then press `Ctrl+o`. |
| `opening the picture…`, `picture shown in the preview`, `picture not opened: <reason>` | The picture is on its way, shown, or refused. |
| `copied chat text`, `nothing to copy: select text or mark a message` | |
| `not set: <reason>` | The channel was not picked; the list stays open and the reason says what to do. |
| `Slack not connected: those Slack codes belong to a different Slack app than the one connected now, ...` | The panel was closed while the codes were checked. Choose `Connect a Slack workspace` in the channel list, paste them again, and choose on the screen that asks. |

## Moving the chat to another computer

*For whoever administers your computers.*

The chat runs on the computer named on the `hub =` line of `hosts.toml`. To move it, change that line and start Ship of
Tools' service (`sotd`, the background program that runs the sessions) on the new computer; nothing else. The old
computer's chat stops within a minute, after filing the one post it is filing, and the new one starts within a minute
after that. A post made in the minute around the move can reach its session as mail up to 6 hours late: the new
computer finds it only at its next check for missed posts, and those checks are never more than 6 hours apart.
