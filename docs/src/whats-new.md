# What's new in 0.7.1

0.7.1 was released on 2026-11-18; 0.7.0 came before it. This page lists what 0.7.1 adds to 0.7.0, and one feature
that 0.7.0 already had, marked so. On a Mac, press Cmd where this page says Ctrl.

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
