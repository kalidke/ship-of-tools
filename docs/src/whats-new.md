# What's new in 0.7.1

0.7.0 is the published release; 0.7.1 is a release candidate. This page lists what the candidate adds to 0.7.0. The
window's bottom border shows the version it runs. Keys are given for Linux and Windows; the macOS equivalents are in
[Keybindings](ref/keybindings.md).

## The project chat

- A session [shows its project's Slack channel](guide/chat.md#What-the-panel-shows) in the Chat drawer (`Ctrl+k`).
- Slack is [connected from the window](guide/chat-setup.md#Connect-your-first-Slack-workspace), and a project's
  channel is picked from a list (`Ctrl+Shift+k`).
- [More than one Slack workspace](guide/chat-setup.md#Connect-another-Slack-workspace) can be connected, and a
  project's channel can be in any of them.
- A message typed in the drawer is
  [posted under the person's own Slack account](guide/chat.md#What-happens-when-you-post).
- [Threads](guide/chat.md#Threads) open in the drawer, and a reply goes into the open thread.
- The file the preview shows [can be posted to the channel](guide/chat.md#Posting-a-file) (`Ctrl+u`).
- [Pictures in a message](guide/chat.md#Pictures) show as tiles and open in the preview (`Ctrl+o`).
- Messages show [Slack's styles, links, mentions and day dividers](guide/chat.md#How-messages-look).
- Message text can be [selected and copied](guide/chat.md#Selecting-and-copying).
- A session's agent can [post text and files to the channel](guide/chat.md#The-agent-in-the-channel).
- Channel posts can [reach the session's agent as mail](guide/chat.md#Mail-to-the-session), off until it is set in
  `chat.toml`.

## Showing the agent a drawing or a selection

- An image in the preview [can be drawn on](guide/drawing.md#Drawing-on-an-image) (`d`), and the session's agent
  sees the marks at its next prompt.
- A [blank canvas](guide/drawing.md#The-canvas) (`Shift+d`) takes a sketch for the agent.
- [Text selected in the preview](guide/drawing.md#Selecting-text) is given to the session's agent at its next prompt.
- Drawing and selection work in Claude Code sessions; for Codex sessions they are
  [not built](guide/drawing.md#What-is-not-built).

## Previews

- [SVG files](guide/previews.md#Built-in-file-types) preview as their picture, with zoom and pan.
- On a computer without poppler or ffmpeg, the first PDF or video preview
  [downloads a copy](guide/previews.md#Built-in-file-types).
- What the preview says about an action shows
  [in a box at the foot of the pane](guide/previews.md#Messages-about-the-preview).

## The window

- Scrolling eases in the preview, the agent pane and the Chat, Terminal and REPL drawers;
  [`smooth_scroll_ms`](ref/config.md#display) sets how long it takes.
- [Each session keeps its own drawer](guide/interface.md#The-bottom-drawer): open or closed, and what it shows.
- Agent output can be [copied as one line](ref/keybindings.md#Default-actions) (`Ctrl+Shift+l`).

## Fixes

- Claude Code sessions run [Claude Code's classic renderer](guide/interface.md#The-Agent-Pane), so the window's scroll
  and selection work in the agent pane.
- A Claude Code session that is cleared (`/clear`) or compacts its context
  [stays joined to the other sessions' messages](guide/messaging.md#Every-session-joins-at-start).
- After a backend starts, [its sessions start a few seconds apart](guide/interface.md#The-lines-around-the-tree), a
  waiting one tagged `[queued]`.
- A message on the [status line](guide/interface.md#The-lines-around-the-tree) stays at least 10 seconds, and the
  line reads `disconnected` while the shown session's computer cannot be reached.
