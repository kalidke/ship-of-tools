# Quickstart

From nothing to an agent running code in your Julia REPL, on one Linux
machine. For other layouts (a remote backend, Windows, building from source)
see [Going remote](remote.md) and [Install details](install.md).

These docs track `main`; the installer installs the latest release (`--version`
to pin one).

## Before you start

- **Linux x86_64** with **glibc 2.35 or newer** (Ubuntu 22.04 or newer) for the
  window. Windows and macOS run as frontends to a Linux backend (macOS is
  experimental). See [Platforms](install.md#platforms).
- A **desktop session with a display** (X11 or Wayland): the window draws
  figures, PDFs and typeset math itself, so it needs a graphical desktop, not
  a terminal. It renders through Vulkan on Linux (Metal on macOS, DirectX 12
  or Vulkan on Windows), so the machine needs a working Vulkan driver.
- **git**, **curl** and **tar**.
- **Claude Code** or **Codex**, installed and logged in. The installer does not
  install either.
- **Julia 1.12** or newer. If Julia is missing, the installer installs it with
  juliaup; an existing juliaup only gets the 1.12 channel added, so if your
  default channel is older, run `juliaup default 1.12` yourself (or set
  `SOT_JULIA_BIN`).
- Optional, on the backend machine: **node/npm** (typeset math in markdown
  previews renders in a node process; without it math is not typeset),
  **poppler-utils** (`pdftoppm` and `pdfinfo`, for PDF previews) and
  **ffmpeg** (the poster frame of a video preview).

!!! warning "Try it in a VM or a separate account"
    The installer changes your own agent setup (skills and hooks into your
    global `~/.claude` and `~/.codex`), and there is no isolated mode yet. See
    [What the installer changes](@ref install-footprint).

## 1. Install

```bash
curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/main/scripts/install.sh | bash -s -- --local
```

Or let an agent do it: start Claude Code or Codex on the machine and say

```text
Install Ship of Tools: fetch https://raw.githubusercontent.com/kalidke/ship-of-tools/main/docs/INSTALL-AGENT.md and follow it.
```

The agent checks the machine, asks where things should run, runs the installer
and checks the result.

`--local` puts the frontend and backend on this machine. The first run
instantiates the Julia environments and takes a few minutes. What the installer
writes, and how to remove it, is listed under
[What the installer changes](@ref install-footprint).

For a first try, the release ships a small demo package (waypoints, distances
and a CairoMakie route plot). Copy it to your home directory and instantiate
it:

```bash
cp -r ~/.local/share/sot/repo/current/docs/fixtures/DemoProject ~/DemoProject
julia --project=~/DemoProject -e 'using Pkg; Pkg.instantiate()'
```

The installer does not instantiate the demo package. This step downloads
CairoMakie and precompiles it, which takes several minutes the first time.

The recordings in these docs also show a few files this demo package does not ship —
an HDF5 file, a PDF and two figures — generated along the way; do not expect
them in a fresh copy.

## 2. Launch

Run `sot-launch`, or open **Ship of Tools** from your desktop's application
menu. The launcher starts the backend if it is not already running, then opens
the window. (With the backend on another machine, the launcher also opens the
SSH connection; see [Going remote](remote.md).) If no window appears, see
[The window does not open](@ref window-does-not-open).

## 3. What you see on first launch

- **Files mode** in the left column, rooted at your home directory, with a
  preview of the file under the cursor in the middle column.
- An **empty agent pane** on the right, titled `llm` (with focus, the title
  shows `Agent` and that pane's keys). Nothing runs there until you create a
  session in the next step.
- A status line reading `connected` at the top of the left column.

`Ctrl+?` shows the focused pane's keys at any time; `F1` opens the searchable
Help drawer.

## [4. Start an agent session](@id start-session)

Codex sessions have no REPL skill yet; the REPL steps below are Claude Code only.

1. Press **`s`** for Sessions mode.
2. Move to **`[+ create new]`** and press **Enter** to open the directory
   picker: a folder tree of the backend machine in the navigation pane,
   starting at your home directory. Move the cursor to your project
   directory, or to `~/DemoProject` if you copied the demo package in step 1;
   `Backspace` goes up a folder and `.` shows hidden folders.
3. Press **Enter** for a Claude Code session, **Ctrl+Enter** (Cmd+Enter on
   macOS) for a Codex session, or **Shift+Enter** for a plain shell with no
   agent.

The session appears as a row in Sessions mode, and the agent starts in the
agent pane with the project directory as its working directory. The first time
Claude Code starts in a folder it asks, in the agent pane, whether you trust
the files in it; answer there once and the session continues. Each session
runs under its own supervisor process on the backend (a *capsule*), so it
keeps running when the window closes.

## 5. Ask the agent to run something

Move focus to the agent pane with `Ctrl+Right`, twice from the navigation
pane (`Ctrl+Arrow` moves focus between panes), and type a request. In the demo project, for example:

```text
In scripts/route.jl, add a bar chart of the distance of each leg, saved as data/legs.png. Run the script in the REPL and show me the new figure.
```

A Claude Code session runs the code in the session's persistent Julia REPL,
the same one you open with `Ctrl+J`, and every run it makes shows up in that
drawer. That REPL starts in the session root's project — `~/DemoProject`'s
`Project.toml` here. The drawer is Ship of Tools' own input line, not Julia's
REPL: no `?` help, no `;` shell and no Tab completion; use the Terminal
drawer or the agent pane for shell commands. When the agent produces a
figure it can put the file in your preview pane. Meanwhile its row colour in
Sessions mode shows whether it is working, waiting for your answer or done; see
[Work-state colours](../concepts/work-state.md).
See [How agents use the REPL](@ref agents-repl).

## 6. End a session, close the window

- In Sessions mode, `Shift+D` pressed twice on a session's row ends that
  session; any other command in between cancels.
- `Ctrl+Q`, with the navigation pane focused, closes the window. The backend
  and its sessions keep running; `sot-launch` opens the window again.

## Next

- [Your first session](tour.md) — the panes, the agent, the drawers and the
  modes, one at a time.
- [Keybindings](../ref/keybindings.md) — every key.
- [Updating and rollback](../guide/updating.md) — re-running the install
  command is also the updater.
