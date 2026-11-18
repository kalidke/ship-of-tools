# What's new in 0.7.1

0.7.1 was released on 2026-11-18; 0.7.0 came before it. This page lists what 0.7.1 adds to 0.7.0. The window's bottom
border shows the version it runs. Keys are given for Linux and Windows; the macOS equivalents are in
[Keybindings](ref/keybindings.md).

## Slack

- A project's Slack channel in a [Chat drawer](guide/chat.md#What-the-panel-shows) (`Ctrl+k`): messages, threads,
  posts.
- [Set up from the window](guide/chat-setup.md#Connect-your-first-Slack-workspace) (`Ctrl+Shift+k`); each project can
  use a channel of a [different workspace](guide/chat-setup.md#Connect-another-Slack-workspace).
- Posts from the drawer are [made from the person's own Slack account](guide/chat.md#What-happens-when-you-post).
- [Pictures in the channel](guide/chat.md#Pictures) show in the drawer; a click or `Ctrl+o` opens one in the preview.
- The previewed file [can be posted to the channel](guide/chat.md#Posting-a-file) (`Ctrl+u`).
- A session's agent can [post text and files to its channel](guide/chat.md#The-agent-in-the-channel).

## Drawing

- An image in the preview [can be drawn on](guide/drawing.md#Drawing-on-an-image) (`d`); a Claude Code session's agent
  sees it with the drawing.
- A [blank canvas](guide/drawing.md#The-canvas) to draw on (`Shift+d`), seen by the agent the same way.

## Session ordering

- The session strip's [order](ref/keybindings.md#Default-actions): by state, alphabetical or by recent activity
  (`Ctrl+Shift+o`). Since 0.7.0.

## Other

- [Smoother scrolling](ref/config.md#display) in the preview, the agent pane and the Chat, Terminal and REPL drawers.
- [Text selected in the preview](guide/drawing.md#Selecting-text) is given to a Claude Code session's agent at its
  next prompt.
- [SVG files](guide/previews.md#Built-in-file-types) preview as pictures.
- PDF pages and video posters
  [preview with no poppler or ffmpeg installed](guide/previews.md#Built-in-file-types).

## Fixed

- New Claude Code sessions stopped at a folder-trust question under Claude Code 2.1.326.
- The window's scrolling did not reach [a Claude Code session's conversation](guide/interface.md#The-Agent-Pane).
- Keys typed while a session connected were lost.
- A line copied from a pane pasted as several lines.
- After a boot, every session started at once;
  [starts are now spaced](guide/interface.md#The-lines-around-the-tree).
- A zoomed PDF page could read "binary file".

## Updating

- A release install on 0.7.0 [downloads 0.7.1 by itself](guide/updating.md#What-updates-a-machine) at its daily
  update check, shows a notice in the window, and applies it at the next launch or backend restart.
- Sessions keep running; the backend restarts, so each session's Julia REPL starts again without its variables.
- 0.7.0 and 0.7.1 work together, but the chat and drawing need 0.7.1 on the computer that runs the sessions: update
  it before the windows that connect to it.
