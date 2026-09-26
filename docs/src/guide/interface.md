# The Interface

Ship of Tools is three columns: navigation, preview and agent. The bottom
drawer, when open, sits under the navigation and preview columns; the agent
column always runs the full height. The columns are fixed in role; the drawer
swaps between four contents. Every pane is keyboard-driven and every chord is
rebindable; see [Keybindings](../ref/keybindings.md).

```@raw html
<DemoShot path="layout-labelled.png" caption="The default layout with the Julia drawer open." />
```

`Ctrl+J`, `Ctrl+T`, `Ctrl+M` and `F1` swap the REPL, Terminal, Monitor and
Help into the drawer. Column order, widths and the drawer height come from the
[layout preset](../ref/config.md#layout); the portrait preset drops the agent
column.

This page walks each pane in turn — what it is, what fills it, and the keys
that drive it — and links out to the guide behind it. For a first-session
walkthrough, see [Your first session](../start/tour.md).

```@raw html
<DemoLoop name="help" caption="The Help drawer (Ctrl+? twice): Tab widens the scope to all panes, typing zoom filters the actions, Down steps through them." />
```

## The panes

| Pane | Position | What it is | Deep dive |
|------|----------|------------|-----------|
| [Navigation](#The-Navigation-Pane) | left column | the mode tree — a collapsible outline whose root you switch with a hotkey | [Modes](modes.md) |
| [Preview](#The-Preview-Pane) | middle column | the selected entry, rendered | [Previews](previews.md) |
| [Agent](#The-Agent-Pane) | right column | the session's agent (Claude Code, Codex or a plain shell) | [Running several agents](agents.md) |
| [REPL](#The-REPL-Pane) | bottom drawer | the persistent Julia session, figures inline | [The REPL](repl.md) |

## The bottom drawer

The bottom drawer is shared by the REPL, the Terminal, the Monitor and Help, each with
its own global toggle. Pressing a second toggle swaps the content in place rather
than stacking:

| Drawer | Toggle | What it is |
|--------|--------|------------|
| [REPL](#The-REPL-Pane) | `Ctrl+J` | persistent Julia session; values, stdout, and figures stream back as structured frames |
| [Terminal](#The-Terminal-Drawer) | `Ctrl+T` | a local OS shell on the *frontend* machine — canonical use is SSHing out to backend hosts |
| [Monitor](#The-Monitor-Drawer) | `Ctrl+M` | CPU / GPU / RAM history across your configured hosts |
| [Help](#Help-drawer) | `F1`, or `Ctrl+?` twice | the focused pane's actions and their bindings, searchable |

Drawer toggles are **global** — they fire even when another pane holds focus.

## Help drawer

**F1** opens the Help drawer for the pane you were using. Type to search, use
arrows to select an action, Tab to browse all panes, and Enter to open its manual.
Escape restores the previous drawer and focus. The Help view preserves its source
context while you read; switching away leaves Julia and terminal processes running.

## Focus and layout

| Action | Key |
|--------|-----|
| Move focus between panes (spatial) | `Ctrl+Arrow` |
| Navigate within the focused pane | plain `Arrow` |
| Maximize the focused pane | `Alt+=` |
| Wide-preview: hide the LLM column, hand its width to the preview | `Alt++` |
| Restore the layout, one layer per press (un-maximize, then exit wide-preview) | `Esc` (only while maximized or in wide-preview) |
| Switch to the next / previous session | `Shift+Arrow` |
| Temporary pane actions / promote to Help drawer | `Ctrl+?` / press again |
| Open Help directly | `F1` |

`Alt+=` maximizes the focused pane, `Escape` restores.

Plain arrows, `Ctrl+Arrow`, and `Shift+Arrow` are scoped so they do not
conflict: `Shift+Arrow` switches sessions, except in a text editor, where it
extends the selection, and in an image preview, where `Shift+Up` and
`Shift+Down` zoom. The authoritative, build-current chord list is in
[Keybindings](../ref/keybindings.md); when in doubt, `Ctrl+?` (or `F1`) is
the source of truth.

## The Navigation Pane

*Left column.* The navigation pane is the **mode tree** — a single collapsible
outline (a parent context, the current level, and its children, shown through
indentation and disclosure carets) that you walk with the arrow keys. It is the
pane you steer from; the [Preview pane](#The-Preview-Pane) reflects whatever node the
cursor is on.

```@raw html
<DemoShot name="nav-files" caption="Files mode with src/DemoProject.jl selected; its source in the preview." />
```

### What fills it

A **mode** is a switchable root for this tree. A hotkey swaps which tree fills the
columns, and **cursor position is preserved per mode**, so jumping between modes
and back lands you where you left each one. The modes bound today:

| Key | Mode | Roots the tree at |
|-----|------|-------------------|
| `f` | Files | the project filesystem |
| `m` | Modules | modules and their definitions (types, functions, macros and submodules), read-only, from `JuliaSyntax.jl` |
| `s` | Sessions | the workspaces this backend is hosting |
| `h` | Hosts | the remote hosts you can target |

The mode keys are plain single characters, so they are **scoped to navigation
focus**: typing `f` into the REPL or a prompt inserts an `f`, it does not switch
modes. The full conceptual mode set (Project, Types, Math, Outputs, Agents) and
what is built today is on the [Modes](modes.md) page.

### Driving it

| Action | Key |
|--------|-----|
| Move up/down the tree | `↑` / `↓` |
| Descend into the cursored node | `→` |
| Back up a level | `←` |
| Switch nav mode | `f` / `m` / `s` / `h` |
| Focus this pane from elsewhere | `Ctrl+Arrow` (spatial) |
| Copy the cursored file's path to the clipboard | `c` |

### The lines around the tree

- **`status:`** above the tree usually reads `connected · <host>:<workspace> · rev N`:
  the connection, the active workspace, and the last revision of backend state
  the window has received. An action such as a pin replaces it with a short
  message (`pinned · <path>`).
- **`annotation:`** below the tree is the [concept annotation](concept-layer.md)
  status of the cursored entry: `present`, `(none)` when no annotation file
  exists for it, `loading`, or `(no target for this row)` for rows such as
  sessions that cannot carry one.
- **`key:`** shows the last key the window received, which helps when a
  binding does not do what you expect.
- In Sessions mode, a tag in brackets after a session's name is the state of
  its supervised process: `[starting]`, `[ready]`, `[ending]`, `[stopped]` (no
  process has run for it yet), `[terminal]` (it ended and will not restart), or
  `[unreachable]` (the backend could not query it). The row is coloured by the
  agent's [work state](../concepts/work-state.md) and ends with the reason the
  agent last reported.
- The bottom border of the window shows the frontend (`fe`) and backend (`be`)
  versions.

Modes are a **planned plugin surface**: the design is a `Mode` subtype with
`tree_root` / `tree_children` / `preview_for` methods — the core modes shipping
the same way, with no privileged path. Today the nav roots are fixed (the four
above) and built into the frontend and backend; the mode-plugin seam is not yet
wired. See
[Writing a Mode Plugin](../extend/mode.md) and
[The Dispatch ABI](../extend/abi.md).


## The Preview Pane

*Middle column.* The preview pane renders whatever the [Navigation pane](#The-Navigation-Pane)
cursor is on, at the fidelity appropriate to its type — markdown rendered, PNGs
shown, `.jl` syntax-highlighted, PDFs paged, video as a poster frame.

```@raw html
<DemoShot name="preview-math" caption="A markdown note with typeset math and a table of each leg's bearing." />
```

### What fills it

The backend first asks the Julia kernel: a `FileType` plugin claims the path
with `matches` and returns a `PreviewPayload` from `preview`. When no plugin
claims it, the backend reads the file itself; that is how raster images and
plain text are served. The payload's `mime` tells the frontend which renderer to
use; the bytes are opaque to Rust, so adding a format is a **Julia-only** change. A sampling of the built-ins:

| Kind | Rendered as |
|------|-------------|
| `.jl` | syntax-highlighted source (tokenized kernel-side, no client re-parsing) |
| `.md` | rendered markdown, including inline math |
| `.json` / `.toml` / `.txt` | plain text (JSON/TOML pretty-print / colour planned) |
| `.pdf` | one rasterized page; with the preview focused (`Ctrl+Right`) `n` / `p` turn pages |
| video (`.mp4`, `.webm`, …) | a poster frame; `o` opens playback in the browser |

The full table — every built-in type and its MIME — is on the
[Previews](previews.md) page.

### Beyond static rendering

- **Pin a file.** `p` keeps a file in the preview pane while you browse
  elsewhere in the tree; `p` again unpins it and the cursor returns to it. A
  pinned preview still updates as the file changes on disk.

```@raw html
<DemoLoop name="pin" caption="Pressing p pins the preview to src/DemoProject.jl while the cursor moves on; pressing p again unpins it; the cursor returns to the pinned file, and the preview then follows the cursor." />
```

- **Opens in the browser, not the pane.** Interactive HTML, Pluto notebooks, and
  Quarto documents pop out to the OS browser through the backend connection, local or remote, rather than
  rendering in-pane; the source still previews as text. Same policy as video.
- **Capture a region for the LLM.** On a raster image you can crop a zoomed region
  and hand it straight to the agent — "what's the artifact here?". The crop
  is taken from the source image on the backend, so the agent sees exactly what
  you do.
- **No silent blanks.** When an external tool is missing (ffmpeg, poppler), the
  plugin returns a `text/markdown` note explaining the gap instead of an empty
  pane.

## The Agent Pane

*Right column.* The agent pane holds the session's agent: Claude Code, Codex or
a plain shell. Its title bar reads `llm`. You describe what you want; the agent
reads code, writes code, and runs code in the [REPL](#The-REPL-Pane) to get
there. See [Agent sessions](orchestrator.md).

### What fills it

Each session runs Claude Code, Codex or a plain shell. A session belongs to a
workspace on the **backend daemon**, so it sits alongside project state and
**survives a frontend restart**.

Several sessions run concurrently — one per workspace/session, across machines — coordinated over the
[inter-agent communication system](messaging.md). You start and switch them
from [Sessions mode](#The-Navigation-Pane) (`s`); cycle the active workspace with
`Shift+Arrow`. What is still deferred is a dedicated in-UI **Agents mode** (a
tasks → timeline → step-detail view).

### How it acts

- **Its own tools, plus a few skills.** The agent brings its own tools and
  context management. The installer adds skills that let it run code in the
  workspace REPL (`sot-fe repl`), show a file in your preview pane
  (`show-result`) and message other sessions, and hooks that report its work
  state. See [How agents use the REPL](@ref agents-repl).
- **Permissions come from the agent CLI.** Sessions launch Claude Code with
  `--permission-mode auto` and Codex with
  `--dangerously-bypass-approvals-and-sandbox --dangerously-bypass-hook-trust`;
  Ship of Tools adds no
  permission layer of its own. See [Agent sessions](orchestrator.md#Permission-mode).

### Hand it a file, or a crop of an image

In Files mode, `c` copies the selected file's path to the clipboard for
pasting into the agent's prompt.

```@raw html
<DemoLoop name="copy" caption="Pressing c in Files mode copies the selected file's path; Ctrl+V pastes it into the Claude Code session in the agent pane; asked what the script does, it answers in one sentence." />
```

With the preview focused (`Ctrl+Right`), `c` on a zoomed image crops the
visible region instead and drops a
ready-to-send line into the agent pane's input, naming the source file, the
region, and the crop saved under `.sot/captures/`.

```@raw html
<DemoLoop name="crop" caption="With the preview focused, pressing c on a zoomed figure sends the visible region to the Claude Code session in the agent pane; asked which leg in it is the shortest, the agent reads the answer off the crop." />
```

## The REPL Pane

*Bottom drawer — `Ctrl+J`.* The REPL pane is a **persistent Julia session**
available throughout a session: the same bindings, the same loaded packages — but
its output is structured, and the figures it produces render **inline in the
native window** rather than as terminal text. It shares the bottom drawer slot
with the [Terminal](#The-Terminal-Drawer) and [Monitor](#The-Monitor-Drawer).

This page covers the pane itself — what fills it, its keys, how to drive it.
For the frame schema, dispatch semantics, and figure-rendering mechanics in
depth, see [The REPL](repl.md).

```@raw html
<DemoShot name="repl-figure" caption="The Julia drawer after running scripts/route.jl: the printed output and the figure inline." />
```

### What fills it

A Julia process **supervised by the backend daemon**, distinct from the kernel
that does project introspection. They are separate on purpose: the kernel owns
dispatch tables, mode trees, indexing, and AST hashing; the REPL owns your
interactive state. Because they are different processes, **killing the REPL does
not kill the kernel** — you can restart your interactive session to clear state or
recover from a wedged computation without losing the project view.

### Driving it

Two ways to drive it, without retyping:

- **Run a `.jl` file** from Files mode — `Shift+R` `include`s it into the current
  session; `r` first resets the REPL into the file's own project, then runs it.
- **Type at the prompt** — `Enter` submits, `Shift+Enter` inserts a newline.

Per-line and per-block dispatch (the line at the cursor, or the surrounding
top-level form) are **planned**, not yet built.

A long-running evaluation runs on its own task, so it does not block the dispatch
loop and you can interrupt it mid-eval — a real `InterruptException`, the same as
`Ctrl-C` in the stock REPL.

### Structured output

Instead of one undifferentiated text stream, the display shim emits **typed
frames** — `stdout`, `stderr`, `value`, `image`, `error`, and a terminal `done` —
so the frontend renders each correctly: `stdout`/`stderr` stream as bytes arrive,
errors carry a structured stacktrace with `file:line` links, and a showable value
(a CairoMakie `Figure`, a `Plots.Plot`, anything with `show(io, MIME"image/png")`)
becomes an `image` frame **drawn inline through the preview layer** — never a
degraded terminal-graphics protocol.

## The Terminal Drawer

*Bottom drawer — `Ctrl+T`.* The Terminal is a **local OS shell on the frontend
machine**. Its canonical use is SSHing outward to backend hosts, and because it is
local — **not** proxied through the daemon — it works even when the backend is
unreachable. It shares the bottom drawer slot with the [REPL](#The-REPL-Pane) (`Ctrl+J`)
and the [Monitor](#The-Monitor-Drawer) (`Ctrl+M`): each key toggles its own content, and
pressing another swaps it in place.

`Ctrl+T` opens the Terminal drawer, a shell in the project root.

| Key | Drawer closed | Showing this content | Showing other content |
|-----|---------------|----------------------|-----------------------|
| `Ctrl+T` | → Terminal | → closed | → Terminal |
| `Ctrl+J` | → REPL | → closed | → REPL |

### Scrollback

Plain `PgUp`/`PgDn` page the drawer's scrollback ring (a one-third-pane step,
the same convention as the LLM pane) — so a `claude` session running here
scrolls exactly like one in the LLM pane. Typing snaps back to the live tail.
Two escape hatches keep full-screen apps working:

- Apps on the **alternate screen** (`vim`, `less`) page themselves — they
  receive the raw key automatically.
- `Shift+PgUp`/`Shift+PgDn` forwards a plain `PgUp`/`PgDn` to a
  primary-screen app that wants the key itself.

The mouse wheel scrolls the same ring, or is forwarded as SGR mouse events
when the running app has enabled mouse tracking (`vim`, `htop`).

## The Monitor Drawer

*Bottom drawer — `Ctrl+M`.* The Monitor shows **CPU / GPU / RAM history across all
your hosts at once** — small-multiples, one compact panel per host (a multi-GPU
box shows each GPU as its own trace). It shares the bottom drawer slot with the
[REPL](#The-REPL-Pane) (`Ctrl+J`) and the [Terminal](#The-Terminal-Drawer) (`Ctrl+T`); the toggle
is global, so it opens even when another pane has focus.


### How it gets the data

The declared **hub** is the aggregator: it alone runs the `[monitor]` roster, and
the drawer subscribes to the hub regardless of which host you're navigating —
"see all servers at once" is solved at the data layer, on the one daemon that's
the monitoring authority. For each host in the roster the hub runs a small
sampler:

- **its own host** locally; **remote hosts** as `ssh <alias> bash -s`, with the
  script fed over stdin — **zero footprint**, no daemon, nothing written to the
  remote's disk.
- CPU% from `/proc/stat`, RAM% from `/proc/meminfo`, GPU from `nvidia-smi`. All
  world-readable, so **no sudo and no privileges** are needed.

A daemon that is **not** the hub samples only the host it runs on — it never ssh's
anywhere, so a wrong or unreachable alias on some other box can't wedge it.
Dialling a non-hub daemon directly shows that daemon's own single panel, not the
fleet; to see the fleet, dial the hub.

Sampling is **always on** for the life of the daemon, so the drawer shows real
history the moment it opens rather than starting from a blank axis; what
`monitor.subscribe` gates is per-connection *delivery* of ticks, not collection.
(ADR 0020 originally specified reactive spawn-on-subscribe; the implementation
went the other way and the ADR carries a note.) The hub keeps an in-memory
tiered ring buffer per host, so the time axis can rescale to wider windows
without a round-trip. Restarting the hub restarts that history.

Which hosts appear comes from the `[monitor]` section of
`~/.config/sot/hosts.toml` (or the file `$SOT_HOSTS` names); see
[Configuration Files](../ref/config.md). With no `[monitor]` section, or on a
non-hub daemon, only that daemon's own host is sampled; the `[host.*]` entries are
frontend connection targets and are not monitored implicitly.

**The list binds when the daemon starts.** Editing `hosts.toml` while the hub is
running changes nothing until it restarts — there is no reload and no file watch.
If the drawer is missing a host you just added, restart the hub.

### On-philosophy rendering

Traces are drawn as **real SVG**, rasterized through the same `resvg` → wgpu-quad
pipeline that renders typeset math — not braille/cell plotting, which is the same
class of degraded hack the project rejects for images. A host whose SSH or sampler
dies renders an explicit **gap**, never a silent flatline or a quiet fallback to
"looks fine."
