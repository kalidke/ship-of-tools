```@raw html
---
aside: false
---
```

# What it does

The main features, with a short recording where one exists. Planned work is on
the [roadmap](design/roadmap.md). Keys are given for Linux and Windows; the
macOS equivalents are in [Keybindings](ref/keybindings.md). What 0.7.1
added is listed in [What's new in 0.7.1](whats-new.md).

```@raw html
<ul class="sot-index">
  <li><strong>Agents and sessions</strong>: <a href="#Claude-Code-and-Codex-sessions">sessions</a> · <a href="#Hand-the-agent-a-file-path">hand the agent a file path</a> · <a href="#Crop-an-image-for-the-agent">crop an image for the agent</a> · <a href="#Selected-text-for-the-agent">selected text for the agent</a> · <a href="#Order-of-the-session-strip">order of the session strip</a></li>
  <li><strong>Slack</strong>: <a href="#A-Slack-channel-in-the-Chat-drawer">a Slack channel in the Chat drawer</a></li>
  <li><strong>Drawing</strong>: <a href="#A-drawing-on-an-image-or-a-blank-canvas">a drawing on an image or a blank canvas</a></li>
  <li><strong>Navigation and previews</strong>: <a href="#Walk-the-project,-see-the-file">walk the project</a> · <a href="#Modules-mode">Modules mode</a> · <a href="#PDF,-paged">PDF</a> · <a href="#Pan-and-zoom-images">pan and zoom</a> · <a href="#Pin-a-file">pin a file</a> · <a href="#Edit-a-file-yourself">edit a file</a></li>
  <li><strong>The REPL</strong>: <a href="#Run-code,-see-figures-inline">run code, see figures inline</a></li>
  <li><strong>The window</strong>: <a href="#Searchable-help">searchable help</a></li>
  <li><strong>Your machines</strong>: <a href="#Your-machines">a remote backend, several frontends, sessions that outlive the connection</a></li>
</ul>
```

```@raw html
<div class="sot-gallery">
```

## Agents and sessions

### Claude Code and Codex sessions

Each agent session is one row in Sessions mode, on this machine or on any
connected host. The row colour is the agent's work state, so you can see which
session needs you without opening each one; the colours are listed in
[Work-state colours](concepts/work-state.md). Codex rows load the Julia REPL skill through the `sot-row` plugin.

```@raw html
<DemoLoop name="sessions" caption="Demo session rows changing colour as a script sets their work states: green working, red waiting for your answer, purple waiting on a job, blue done, gray idle. Each row's ready tag means its supervisor has a live agent process running." />
```

### Hand the agent a file path

In Files mode, `c` copies the selected file's path to the clipboard for
pasting into the agent's prompt.

```@raw html
<DemoLoop name="copy" caption="Pressing c in Files mode copies the selected file's path; Ctrl+v pastes it into the Claude Code session in the agent pane; asked what the script does, it answers in one sentence." />
```

### Crop an image for the agent

With the preview focused (`Ctrl+Right`), zoom into an image, then `c` crops
the visible region and drops a ready-to-send line into the agent pane, naming
the source file and the region, with the crop attached.

```@raw html
<DemoLoop name="crop" caption="With the preview focused, pressing c on a zoomed figure puts the visible region into the agent's input, and the question is sent: asked which leg in it is the shortest, the agent reads the answer off the crop." />
```

### Selected text for the agent

Text selected with the mouse in a markdown, source or plain text preview is
given to the session's agent at its next prompt, labeled with the file and its
line numbers. Text selection is built for Claude Code sessions and for Codex sessions on Unix.
Delivery in a real Codex session still needs checking. See
[Selecting text](guide/drawing.md#Selecting-text).

### Order of the session strip

`Ctrl+Shift+o` cycles the order of the session strip along the bottom edge:
by state, alphabetical, or by recent activity. A session's recent activity
is the latest of when it was created, when something was last typed into it
(by you in any window, or by another session), and when its agent last
reported: at a turn's start and end, at each state it declares, about once
a minute while it works, and when a prompt that waits for you (a permission
prompt, say) appears in it or goes away. So a session you start from this
window is first, unless other sessions reported or were typed into in the
same second or later: it is then behind them. A session that someone else
starts (an agent, another window) while you are on the first one appears
behind it, since the session you are on keeps its place. Typing in a session
with no agent moves it up as an agent's work moves an agent session. What a
program prints does not count, and neither does a message sent to a session
by itself: it counts when it is typed into the session, as it is for a
Claude Code session waiting at its prompt. A Codex session at rest is asked
to read its messages without anything being typed into it; the turn that
follows reports as any turn does, which should move it, and that has not yet
been checked in a real Codex session. A message to a session that is busy,
or one that cannot be woken just then, moves nothing. The session you are looking
at keeps its place while you type and takes its new place when you leave
it; a time can be up to ten seconds behind the last key, counted while the
computer is awake: after it has slept, the first key typed in a session
within those ten seconds may not count, and the next one after them does.
Typing times are not kept on disk: they
are forgotten when the computer restarts and when closing the last window
ends the sessions, and the order then follows creation and the agents'
reports until something is typed again. A session that was already running
when Ship of Tools was updated counts typing only once the session itself
has ended and started again, as it does at a restart of the computer; a new
agent started inside a session that kept running is not enough.

### More on agents

- **Agents run code in your REPL.** Claude Code sessions run Julia in the
  session's persistent REPL, and each
  run appears in your REPL drawer. See [How agents use the REPL](@ref agents-repl).
- **Agents can show you results.** A session can open an image or file in your
  preview pane instead of describing it in text.
- **Agents messaging each other.** A built-in message relay (the comm relay)
  carries directed or broadcast messages between sessions, across machines; the same channel
  drives the row colours above. See
  [Agents messaging each other](guide/messaging.md).

## Slack

### A Slack channel in the Chat drawer

`Ctrl+k` shows the Chat drawer (its guides call it the Chat panel): the Slack
channel of the selected session's project, newest message at the bottom. A
message typed there is posted from the person's own Slack account, and
`ArrowRight` opens the marked message's thread. Slack is set up in the drawer
itself (`Ctrl+Shift+k`), and each project can use a channel of a different
workspace. See [Setting up Slack](guide/chat-setup.md) and
[The project chat](guide/chat.md).

### More on Slack

- **Pictures.** A message's pictures show in the drawer; a click on one, or
  `Ctrl+o` in the drawer, opens it in the preview. See [Pictures](guide/chat.md#Pictures).
- **The previewed file.** From the drawer, `Ctrl+u` posts the file the
  preview shows to the channel. See [Posting a file](guide/chat.md#Posting-a-file).
- **Posts from the agent.** A session's agent posts text and files to its
  project's channel with `sot-fe chat post`. See
  [The agent in the channel](guide/chat.md#The-agent-in-the-channel).

## Drawing

### A drawing on an image or a blank canvas

With the preview focused (`Ctrl+Right`), `d` turns drawing on over an image:
the left mouse button draws and the right one erases. The session's agent is
given the image with the drawing on it at its next prompt. `Shift+d` shows a
blank canvas to draw on, which the agent is given the same way. Drawings reach
Claude Code sessions and, on Unix, Codex sessions. See [Drawing and selecting for the agent](guide/drawing.md).

## Navigation and previews

### Walk the project, see the file

In Files mode the cursor walks the tree and the preview pane follows it:
markdown with typeset math, an HDF5 tree, a paged PDF, a figure.

```@raw html
<DemoLoop name="navigate-g" caption="The preview follows the cursor through notes with math, the agent's bar chart, an HDF5 file, a PDF and the route figure." />
```

### Modules mode

`m` switches the navigation tree to Modules mode — modules and their
definitions — and the preview shows each definition's docstring and source as
the cursor moves. Read-only, derived from `JuliaSyntax.jl`. See
[Modes](guide/modes.md).

```@raw html
<DemoLoop name="modules" caption="Modules mode: the cursor moves through a module's definitions, with docstring and source in the preview." />
```

### PDF, paged

With the preview focused (`Ctrl+Right`), `n` pages a PDF forward and `p` back;
each page rasterizes on the backend to fit the pane.

```@raw html
<DemoLoop name="pdf" caption="With the preview focused, n and p page through a PDF, then = zooms in to read it." />
```

### Pan and zoom images

With the preview focused (`Ctrl+Right`), `=` (or `Shift+Up`) zooms an image
in and the arrow keys pan it. Same-size images in a directory share zoom and pan, so stepping
through a run's plots keeps your framing.

```@raw html
<DemoLoop name="zoom" caption="With the preview focused, = zooms an image and the arrow keys pan; the next same-size image opens at the same zoom and pan." />
```

### Pin a file

In Files mode, `p` pins a file to keep it in the preview pane while you browse
elsewhere; it still updates as it changes on disk.

```@raw html
<DemoLoop name="pin" caption="Pressing p pins the preview to src/DemoProject.jl while the cursor moves on; pressing p again unpins it; the cursor returns to the pinned file, and the preview then follows the cursor." />
```

### Edit a file yourself

With the preview focused (`Ctrl+Right`), `e` opens the displayed file in the
built-in editor in the preview pane; `Ctrl+s` saves and `Escape` closes it,
asking first if there are unsaved edits. When the entry has a concept
annotation (a note about it, written by you or the agent and kept under
`.concept/`), `e` edits the annotation instead; see
[The concept layer](guide/concept-layer.md).

In the editor, `ArrowUp` and `ArrowDown` move by the row shown on screen, so in
a paragraph the pane wraps over several rows they step through those rows, and
the cursor keeps its column across a shorter row. `PageUp` and `PageDown` move
a third of the pane's rows the same way. `Home` and `End` go to the start and
end of the whole line of the file, which for a wrapped paragraph is the
paragraph. Hold `Shift` with any of these to extend the selection. A row of
right-to-left text is the exception: a step onto or off it goes by the file's
line. The cursor is a thin bar drawn over the text, on every row, at the edge of
the letter it stands before, in the text's color, and the selection is a dim
gold band under the selected text, dark enough that the text stays easy to read. A line end written as CR and LF is one position
for the cursor, and a CR alone ends a line as a line feed does. The view follows the cursor: when
a key moves it above or below the pane, the text scrolls just far enough to
show it again, at once. The mouse wheel can still scroll the text away from the cursor,
and the next key that moves the cursor or changes the text brings it back.

A save is written inside the project and never through a link: a file whose
own name is a link, or one reached through a linked folder, is refused, and
your edits stay in the editor. When the link leads to a place inside the
project, open the file there to save it; a file that a link leads to outside
the project cannot be saved from the window. A new folder follows the same
rule.

There is no diff view of its own: you review the agent's edits in the preview
pane, which follows a file as it changes on disk, in the REPL drawer, which
shows every run the agent made, and with `git` in a shell session.

### More on previews

- **Add a file type in Julia.** A `FileType` subtype, a `matches` method and a
  `preview` method; no Rust changes. See [The dispatch ABI](extend/abi.md).
- **SVG files preview as pictures.** An `.svg` file is drawn to fit the pane,
  and zooms and pans as a PNG does. See
  [Built-in file types](guide/previews.md#Built-in-file-types).
- **PDF pages and video posters need no installed tools.** On a computer
  without poppler or ffmpeg, the first such preview downloads a copy. See
  [Built-in file types](guide/previews.md#Built-in-file-types).
- **Video and interactive documents open in the browser.** Video shows a
  poster frame in the pane; `o` opens playback in your browser. Pluto
  notebooks, Quarto documents and HTML pages follow the same policy, served
  through the backend connection, local or remote, with no SSH setup.

## The REPL

### Run code, see figures inline

A persistent Julia REPL per session. Run a whole `.jl` file (`r` into a
fresh REPL — this also clears what the agent built in `Main` — `Shift+r`
into the current session) or type at the prompt.
CairoMakie figures render inline in the drawer as images, not through a
terminal graphics protocol. See [The REPL](guide/repl.md).

```@raw html
<DemoLoop name="repl-g" caption="Asked to run scripts/route.jl, the agent runs it in the shared Julia drawer; the output and the leg-distance bar chart land inline, then it answers." />
```

The drawer's REPL and the agent's are the same Julia process and `Main`, one
evaluation at a time; see [How agents use the REPL](guide/repl.md#agents-repl).

- **Interactive figures open in your browser.** `wglshow(fig)` serves a live
  WGLMakie figure in your browser so you can pan, zoom and rotate in 3-D;
  the page and its WebSocket are served through the backend connection,
  local or remote, with no SSH setup.

## The window

### Searchable help

`Ctrl+?` overlays the focused pane's actions; press it again for the
searchable Help drawer, `Tab` widens it to every pane, and typing narrows the
list.

```@raw html
<DemoLoop name="help" caption="The Help drawer (Ctrl+? twice): Tab widens the scope to all panes, typing zoom filters the actions, Down steps through them." />
```

- **A local terminal.** `Ctrl+t` opens the Terminal drawer, an OS
  shell on the frontend machine, typically used to SSH out to backend hosts.
- **Fill the window.** `Alt+=` maximizes the focused pane,
  `Escape` restores.

### More on the window

- **Watching the machines.** The Monitor drawer (`Ctrl+m`) shows CPU, GPU and
  memory across your Linux hosts, for example while a training run is going.
- **Smoother scrolling.** A scroll in the preview, the agent pane and the
  Chat, Terminal and REPL drawers arrives over a short time instead of
  jumping; `smooth_scroll_ms` under `[display]` sets how long, and `0` moves
  at once. See
  [Configuration files](ref/config.md#display).
- **Keybindings.** Named actions can be rebound in a keybindings file;
  anything you do not override keeps its default. See
  [Keybindings](ref/keybindings.md) for the lookup order and every key.
- **Configuration.** Layout presets for ultrawide, laptop and portrait screens,
  and a `[gpu]` setting for hybrid-graphics laptops; see
  [Configuration files](ref/config.md).
- **More keys.** Uploads and downloads, an image scalebar, text scaling and a
  printable cheat sheet of every key are on the [Keybindings](ref/keybindings.md) page.

## Your machines

The setup is in [Going remote](start/remote.md).

- **A remote backend.** The backend daemon runs where your GPUs and data are;
  the window runs on your laptop or desktop and reaches it over SSH. The
  backend can also run locally, on Linux. Windows and macOS run as frontends
  to a Linux backend (macOS is experimental). See
  [Platforms](start/install.md#platforms).
- **Several frontends.** A desktop and a laptop can attach to the same backend
  at once and see the same sessions, work-state colours and REPLs. See
  [A second frontend](guide/second-frontend.md).
- **Sessions outlive the connection.** Agent sessions run under their own
  supervisor, and their REPLs under the backend daemon, so a dropped
  connection stops nothing, and with a remote backend neither does closing the
  laptop lid or the window; the next launch reattaches. Closing the last window
  on the computer that runs the backend ends its sessions; `Ctrl+q` can keep them.
  See [Sessions and persistence](concepts/sessions.md).

```@raw html
</div>
```
