# What Ship of Tools Can Do

Ship of Tools is an opinionated, **agentic** development system for the Julia language.
You drive **Claude Code** agents from the keyboard — they write and run the
code; you steer, watch, and review. This page is the tour of what that buys you.

![Ship of Tools main window](assets/screenshots/hero.png)
*Fullscreen on an ultrawide display — the layout Ship of Tools is designed around: all four panes comfortably side by side.*

## Built for ultrawide

Ship of Tools is **opinionated about screen real estate**: the four-pane layout is
designed for a single ultrawide monitor, where navigation, preview,
orchestrator, and REPL all get useful width at once. It runs fine on 16:9,
but ultrawide is the layout it is *for* — maximize a pane (`Alt+=`) when you
need depth over breadth.

## Drive agents, don't type code

- Ship of Tools is **Claude Code–centric**: the agents you direct are Claude Code
  sessions. You orchestrate them from the keyboard instead of typing code
  yourself.
- **Run several at once.** Multiple agents work concurrently — one per
  workspace/session, across machines — coordinated over the inter-agent comm bus
  (below) and surfaced in the Sessions view. You spawn and drive them hands-on
  today; a dedicated in-UI orchestration timeline ("Agents mode") is on the
  [roadmap](design/roadmap.md).
- **See who needs you.** Each agent reports a live state — **idle** (gray),
  **working** (green), **blocked** (a hard block needing your decision; red),
  **waiting** (a soft await on a peer or async result; purple), or **done**
  (blue) — driven by Claude Code hooks plus explicit status calls, so you can
  scan every session's color at a glance instead of tabbing through each one.
- **Review the work.** Review what each agent changed in its session and with
  git. (Per-edit provenance colouring and in-UI **accept or reject** are
  planned — see [Color Coding](guide/color-coding.md).)
- **Agents drive the UI, too.** Skills and hooks let a Claude Code session open
  an image or file straight in your nav/preview pane to show you a result it
  just produced — not just describe it in text.
- **Ask it for help.** The in-pane Claude Code session doubles as your help
  system: ask a how-to question about Ship of Tools itself and it answers from
  the project's own in-repo docs — no separate manual to go find.
- **Hand the agent a file path.** In the file nav, `c` copies the cursored
  file's full path to the clipboard — paste it straight into the agent's prompt.

## Agents coordinate across machines

- A built-in **inter-agent communication system** lets sessions message one
  another — directed or broadcast — across separate sessions and across machines
  on a shared filesystem.
- Each agent's state (idle / working / blocked / waiting / done) propagates over
  that same channel, which is what powers the live status indicators.
- It is how a backend agent on a remote server and a frontend session on your
  laptop stay aware of each other.

## Work where your compute is

- The backend runs **local** (WSL / Linux / macOS\*) or on a **remote server
  over SSH** — put it where your GPUs and data live.
- **Close your laptop and walk away** — the backend, REPL, and agents keep
  running on the remote.
- Reopen and Ship of Tools **auto-reconnects**, restoring your session and view.
- Connect **multiple frontends** (laptop + desktop) to the same backend.
- Switch **workspaces** (projects) and **hosts** (machines) from inside the app.
  See [Backend & Sessions](design/backend.md).

## Keyboard-driven, and yours to remap

- **Everything from the keyboard.** Navigate the tree, switch panes and modes,
  and act on the cursored entry — the whole interface is operable without ever
  reaching for the mouse.
- **Remap any chord.** Every named action can be rebound: per-repo in
  `.sot/keybindings.toml`, or per-user in `$HOME/.config/sot/keybindings.toml`;
  anything you don't override falls through to the defaults. See
  [Keybindings](ref/keybindings.md).
- **Full-screen any pane** — maximize the pane you're in (`Alt+=`) when you
  need depth over breadth.

## Live Julia, in the loop

- A **persistent Julia REPL** — run a whole `.jl` file (in a fresh REPL, or
  `include`d into the current session), or type at the prompt. Per-line and
  per-block dispatch are planned, not yet built.
- **Plots render inline** in the window (CairoMakie) — not dumped as text or
  squeezed through a terminal graphics hack. See [The REPL](guide/repl.md).
- **Interactive figures pop out to your browser** — `wglshow(fig)` serves a live
  WGLMakie figure over an auto-forwarded port and opens it in your real browser,
  so you can pan, zoom, and rotate 3-D; the interaction WebSocket rides the same
  forward, local or remote. CairoMakie stays the tool for static inline plots.
- Open and interact with **Pluto notebooks**; render **Quarto** and **HTML**
  (popped out to your browser when that is the right surface).
- **Extensible previews, no Rust required.** Teach it a new file type in a few
  lines of Julia — a subtype, a `matches` predicate, a `preview` method — using
  ordinary multiple dispatch. See [The Dispatch ABI](extend/abi.md).

## See your work, at full fidelity

- **Native previews** of images (PNG), **PDF** (paged, rasterized in-pane),
  markdown, JSON, syntax-highlighted source, and typeset **math** — drawn by
  Ship of Tools itself, never truncated to text. See [Previews](guide/previews.md).
- **Pan and zoom figures.** Zoom into any image preview; same-size figures in a
  directory share the zoom and pan, so stepping through a run's plots keeps
  your framing instead of resetting it each time.
- **Movies and animations open in your browser** — the preview pane shows a
  poster frame, and a keypress pops playback out to a real HTML5 player. Pluto,
  Quarto, and HTML follow the same browser policy.
- **Serve a web page locally.** Point it at any on-disk static site — a
  Documenter build, a coverage report, a notebook export — and open it in your
  real browser over an auto-forwarded port, one key chord away.
- **Pin a file to a live view** that updates as it changes on disk.
- **Capture an image region for the agent.** Zoom into a plot or image preview,
  then press `c` to crop the visible region and hand it straight to the in-pane
  agent.

## Keep an eye on the machines

- A **monitoring drawer** shows CPU / GPU / memory across your hosts — built for
  watching training runs.
- A built-in **terminal pane** (a real PTY) for when you just need a shell.

## Make it yours

- **Lay it out for your screen** — a four-pane layout you configure for your
  screen, with aspect-ratio presets (ultrawide / laptop / portrait). It is at
  its best on an **ultrawide** display, and fully usable on a **laptop or 4K**
  screen. See [Configuration Files](ref/config.md).
- **Rebuild the frontend without losing your session.**

<small>\* macOS support is wired through the release installer but remains experimental.</small>
