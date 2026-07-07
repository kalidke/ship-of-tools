# A Guided Tour

This is a first-session walkthrough: launch the app, move through the modes and
drawers, and learn the handful of keys that drive everything. Ship of Tools is
keyboard-only by design — no mouse needed. The keys below are the defaults from
`.sot/keybindings.toml`; every one is rebindable (see
[Keybindings](../ref/keybindings.md)).

If you have not started Ship of Tools yet, do [Per-Machine Setup](setup.md) and read
[Running & Relaunch](running.md) first.

## The window

The window is four panes plus a shared bottom drawer:

```text
┌────────────────────────┬────────────────────────┐
│ navigation (mode tree) │ preview (file fidelity) │
├────────────────────────┼────────────────────────┤
│ orchestrator / pty     │ REPL                    │
└────────────────────────┴────────────────────────┘
```

The top-left pane is the **mode tree** — a collapsible outline
whose root you switch with a hotkey. The top-right pane is the **preview**,
rendering whatever you have selected at full fidelity. The bottom row holds the
orchestrator / terminal and the REPL.

## Launch

Start the app through your launcher (see [Running & Relaunch](running.md)). The
frontend comes up, connects to the backend, and shows the first mode.

## Files mode — `f`

Press `f` for **Files mode**: filesystem navigation over the project. Move with
the arrow keys — `↑` / `↓` to move within a column, `→` to descend into a
directory, `←` to go back up. As you move, the **preview pane** renders the
selected file at the right fidelity: markdown rendered, PNGs shown, `.jl` files
syntax-highlighted, and PDFs and video too. Nothing is reduced to plain text.

For what each file type renders and how, see [Previews](../guide/previews.md).

## Modules mode — `m`

Press `m` for **Modules mode**: a structural view of the code, derived
mechanically from `JuliaSyntax.jl` — modules, then their functions, then
methods. It is read-only in phase 1. Cursor position is preserved per mode, so
switching `f` ↔ `m` returns you to where you were in each.

The full set of modes and their column shapes is in [Modes](../guide/modes.md).

## The REPL drawer — `Ctrl+J`

Press `Ctrl+J` to open the **REPL** in the bottom drawer: a persistent Julia
session that lives for the whole session. From Files mode you can run a whole
`.jl` file in it without retyping — `r` runs it in a fresh REPL, `R` includes
it in the current one. At the prompt, type an expression and press Enter to run
it (Shift+Enter inserts a newline); outputs (values, stdout, errors, even
images) stream back as structured frames.

For file dispatch and the display protocol, see
[The REPL](../guide/repl.md).

## The Terminal drawer — `Ctrl+T`

Press `Ctrl+T` for a local **Terminal** — an OS shell on the frontend machine,
typically used to SSH out to backend hosts. The REPL and Terminal share one
physical drawer slot: `Ctrl+J` and `Ctrl+T` each toggle their own pane, and
pressing the other key swaps the content. See
[Running & Relaunch](running.md#the-terminal-drawer).

## The Monitor drawer — `Ctrl+M`

Press `Ctrl+M` for the **Monitor** drawer — a server monitor sampling the
configured hosts (GPU and process stats from `nvidia-smi` and `/proc`, which are
world-readable, so no privileges are needed). Which hosts appear comes from the
`[monitor]` section of `.sot/hosts.toml`.

## Navigation modes: Sessions — `s` — and Hosts — `h`

Two more nav-tree roots switch the *target*, not the content:

- **`s` — Sessions.** Pick a directory and commit it as a workspace. `Enter`
  starts the comm-aware agent session in the pane; `Shift+Enter` starts a bare
  session with no LLM agent (a plain shell / REPL).
- **`h` — Hosts.** List every backend host from `.sot/hosts.toml`, with the
  current and default hosts badged. `Enter` picks a host; the choice persists,
  and the next launch and any reconnect target it. Switching hosts is "pick →
  `Ctrl+Q` → relaunch".

## Help — `?`

Press `?` (in nav or preview focus, outside edit mode) for the in-app help
overlay — the live, authoritative keymap for your build, including any rebinds.
When in doubt, `?` is the source of truth.

## Layout and focus

A few global keys manage the window itself:

| Action | Key | What it does |
|--------|-----|--------------|
| Move pane focus | `Ctrl+Arrow` | shift focus spatially between the four panes |
| Cycle workspace | `Shift+Arrow` | switch the active workspace (left / right) |
| Maximize pane | `Alt+=` | blow the focused pane up to fill the window |
| Restore layout | `Esc` | un-maximize (only while a pane is maximized) |
| Font scale | `Ctrl+=` / `Ctrl+-` | zoom the UI font up / down (`Ctrl+0` resets) |
| Reconnect | `F5` | reconnect the transport after a drop |
| Quit | `Ctrl+Q` | real quit (nav focus only) |

Plain arrows nav within a pane, `Ctrl+Arrow` moves *between* panes, and
`Shift+Arrow` cycles workspaces — three disjoint roles, so they never collide.

## Where to go next

- [Architecture at a Glance](../guide/architecture.md) — the three processes and
  why the language split.
- [Modes](../guide/modes.md) and [Previews](../guide/previews.md) — the nav
  surface in depth.
- [The REPL](../guide/repl.md) and [The Orchestrator](../guide/orchestrator.md) —
  running code and driving the LLM.
- [The Dispatch ABI](../extend/abi.md) — teach Ship of Tools a new file type or mode.
