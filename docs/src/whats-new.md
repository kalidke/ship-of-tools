# What's new in 0.7.1

0.7.1 was released on 2026-11-18; 0.7.0 came before it. This page lists what 0.7.1 adds to 0.7.0, and one feature
that 0.7.0 already had, marked so. On a Mac, press Cmd where this page says Ctrl.

## 0.7.3

0.7.3 was released on 2026-11-21. It adds these to 0.7.2:

- A drawer's own key now cycles it. From closed, `Ctrl+k` opens the Chat drawer, a second press makes it full
  height, and a third closes it; the REPL, Terminal and Monitor drawers do the same with their keys. At full height the status
  line shows on the drawer's top border, and each session keeps its own drawer and its height. See
  [the interface guide](guide/interface.md).
- When you step back to a session, its Chat drawer shows its channel's last messages at once while it reads the
  channel again, and you can type straight away.
- A name after `@` in a chat post [mentions that person in Slack](guide/chat.md#Mentioning-someone), and the status
  line tells you who was mentioned. A first name is enough when only one member of the Slack workspace has it.
- A session's agent can be asked to [post to its project's channel](guide/chat.md#The-agent-in-the-channel) and now
  knows how without being told the command.
- In the preview's edit mode, Up and Down move by the line you see on screen, also inside a long wrapped paragraph.
  Typing in a large file is immediate, and a selection is a dim band with the text readable on it.
- The [REPL drawer](guide/repl.md) uses the colors of Julia's own REPL, wraps long lines, and shows the colors a program
  prints. A progress meter that repaints its line updates in place. The same page lists what the drawer does not do
  with a program's output.

Fixed:

- On Windows, the launcher did not start for a user whose name has a space in it.
- When a workspace's chat stopped by itself, the Chat drawer could show `reading…` for a few seconds where the
  Connect step belongs.
- Escape on an edited file now shows its discard question, and a discard shows the file as last saved.

A computer gets 0.7.3 by [running the installer](guide/updating.md#updating) once.

## 0.7.2

0.7.2 was released on 2026-11-19. It is 0.7.1 with two faults fixed, both on a Mac; Linux and Windows behave as in
0.7.1.

- On a Mac running macOS 27, 0.7.1 opened no page in a browser: the browser showed that the server had dropped the
  connection. Pages open again.
- On a Mac, a window started from the Applications icon left no log to read. It now
  keeps its log in a file; [Troubleshooting](guide/troubleshooting.md) says where.

A Mac on macOS 27 gets both by [running the installer](guide/updating.md#updating) once.

## Slack

- A project's Slack channel in a [Chat drawer](guide/chat.md#What-the-panel-shows) (`Ctrl+k`): messages, threads,
  posts.
- The chat is [set up from the window](guide/chat-setup.md#Connect-your-first-Slack-workspace) (`Ctrl+Shift+k`); each
  project can use a channel of a [different workspace](guide/chat-setup.md#Connect-another-Slack-workspace).
- A post from the drawer [appears in the channel as the person's own message](guide/chat.md#What-happens-when-you-post),
  from their Slack account.
- [Pictures in the channel](guide/chat.md#Pictures) show in the drawer; a click on one, or `Ctrl+o` in the drawer,
  opens it in the preview.
- From the drawer, `Ctrl+u` [posts the previewed file to the channel](guide/chat.md#Posting-a-file).
- A session's agent, when asked, can
  [post text and files to its project's channel](guide/chat.md#The-agent-in-the-channel).

## Drawing

- An image in the preview [can be drawn on](guide/drawing.md#Drawing-on-an-image) (`d`); a Claude Code session's agent
  is given the image with the drawing on it at the next prompt.
- A [blank canvas](guide/drawing.md#The-canvas) to draw on (`Shift+d`), given to the agent the same way.

## Session ordering

- Already in 0.7.0: the session strip, the row of session names above the window's bottom border, has three
  [orders](ref/keybindings.md#Default-actions), by state, alphabetical and by recent activity; `Ctrl+Shift+o` moves
  to the next.

## Other

- [Smoother scrolling](ref/config.md#display) in the preview, the agent pane and the Terminal and REPL drawers; the
  Chat drawer scrolls the same way.
- [Text selected in the preview](guide/drawing.md#Selecting-text) is given to a Claude Code session's agent at its
  next prompt.
- [SVG files](guide/previews.md#Built-in-file-types) preview as pictures.
- PDF pages and a video's poster frame preview on a computer with no poppler or ffmpeg installed:
  [Julia's copy is downloaded once](guide/previews.md#Built-in-file-types), at the first such preview.

## Fixed

Faults fixed since 0.7.0:

- A new Claude Code session stopped at a question about trusting a folder when Claude Code was at version 2.1.326.
- The agent pane could not scroll [a Claude Code session's conversation](guide/interface.md#The-Agent-Pane) when
  Claude Code used its fullscreen display.
- Keys typed in the agent pane while the window was connecting to a session were lost.
- A long line copied from the agent pane pasted as several lines, broken where the pane had wrapped it.
- When the computer that runs the sessions started up, all its sessions started at once; they now
  [start a few seconds apart](guide/interface.md#The-lines-around-the-tree).
- A zoomed PDF page could show the words "binary file" in place of the page.

## Updating

- Each computer on 0.7.0 [downloads 0.7.1 by itself](guide/updating.md#What-updates-a-machine), and the window shows
  a notice.
- A computer changes to the downloaded version the next time the Ship of Tools window is launched on it, or its
  service is restarted.
- [Running the installer](guide/updating.md#updating) updates a computer without waiting for that.
- Sessions keep running through the update; each session's Julia REPL starts again, without its variables.
- A session started on 0.7.0 is given a drawing or a selection only after it is
  [ended and started again](guide/drawing.md#When-the-window-says-held).
- The chat, a drawing and a selection need 0.7.1 on the computer that runs the sessions and on the computer with the
  window.
- The window's bottom border reads `fe <version> · be <version>`: the window's version, then that of the service that
  runs the shown session.
