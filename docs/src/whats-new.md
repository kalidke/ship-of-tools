# What's new in 0.7.1

0.7.1 was released on 2026-11-18; 0.7.0 came before it. This page lists what 0.7.1 adds to 0.7.0, and one feature
that 0.7.0 already had, marked so. An item that works only in Claude Code sessions says so. On a Mac, press Cmd where
this page says Ctrl.

## Slack

- A project's Slack channel in a [Chat drawer](guide/chat.md#What-the-panel-shows) (`Ctrl+k`): messages, threads,
  posts.
- The chat is [set up from the window](guide/chat-setup.md#Connect-your-first-Slack-workspace) (`Ctrl+Shift+k`); each
  project can use a channel of a [different workspace](guide/chat-setup.md#Connect-another-Slack-workspace).
- Posts from the drawer are [made from the person's own Slack account](guide/chat.md#What-happens-when-you-post).
- [Pictures in the channel](guide/chat.md#Pictures) show in the drawer; a click or `Ctrl+o` opens one in the preview.
- The previewed file [can be posted to the channel](guide/chat.md#Posting-a-file) (`Ctrl+u`).
- A session's agent, when asked, can
  [post text and files to its project's channel](guide/chat.md#The-agent-in-the-channel).

## Drawing

- An image in the preview [can be drawn on](guide/drawing.md#Drawing-on-an-image) (`d`); a Claude Code session's agent
  is given the image with the drawing on it at the next prompt.
- A [blank canvas](guide/drawing.md#The-canvas) to draw on (`Shift+d`), given to the agent the same way.

## Session ordering

- Already in 0.7.0: the session strip, the row of session names along the window's bottom edge, is
  [ordered](ref/keybindings.md#Default-actions) by state, alphabetically or by recent activity (`Ctrl+Shift+o`).

## Other

- Smoother scrolling: a scroll [glides to its place](ref/config.md#display) in the preview, the agent pane and the
  Terminal and REPL drawers.
- [Text selected in the preview](guide/drawing.md#Selecting-text) is given to a Claude Code session's agent at its
  next prompt.
- [SVG files](guide/previews.md#Built-in-file-types) preview as pictures.
- PDF pages and a video's poster frame
  [preview on a computer with no poppler or ffmpeg installed](guide/previews.md#Built-in-file-types), where 0.7.0
  showed a note naming the missing tool.

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

- A computer on 0.7.0 [downloads 0.7.1 by itself](guide/updating.md#What-updates-a-machine) within a day and shows
  a notice in the window; the next start of Ship of Tools on that computer switches to it.
  [The installer](guide/updating.md#updating) updates a computer on request.
- The switch keeps sessions running and starts each session's Julia REPL again, without its variables; a session
  started on 0.7.0 is given a drawing or a selection only after it is ended and started again.
- The chat, a drawing and a selection need 0.7.1 on the computer that runs the sessions as well as on the window's;
  the window's bottom border shows both versions, `fe` for the window and `be` for that computer.
