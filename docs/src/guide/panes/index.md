# The Panes

Ship of Tools is a **four-pane** window over a **shared bottom drawer**. The panes
are fixed in role; the drawer swaps between three contents. Every pane is
keyboard-driven and every chord is rebindable — see
[Keybindings](../../ref/keybindings.md).

```text
┌────────────────────────┬────────────────────────┐
│ navigation (mode tree) │ preview (file fidelity) │
├────────────────────────┼────────────────────────┤
│ orchestrator / pty     │ REPL                    │
└────────────────────────┴────────────────────────┘
```

The bottom-right slot is a **drawer**: `Ctrl+J` / `Ctrl+T` / `Ctrl+M` swap the
REPL, Terminal, and Monitor into it.

This section has one page per pane. Each is a short orientation — what the pane
is, what fills it, and the keys that drive it — and links out to the in-depth
guide page behind it. For the first-session walkthrough, see
[A Guided Tour](../../start/tour.md).

## The four panes

| Pane | Position | What it is | Deep dive |
|------|----------|------------|-----------|
| [Navigation](navigation.md) | top-left | the mode tree — a collapsible outline whose root you switch with a hotkey | [Modes](../modes.md) |
| [Preview](preview.md) | top-right | the cursored entity rendered at full fidelity | [Previews](../previews.md) |
| [Orchestrator](orchestrator.md) | bottom-left | the agent session (Claude Code) that drives the project | [The Orchestrator](../orchestrator.md) |
| [REPL](repl.md) | bottom-right | the persistent Julia session, figures inline | [The REPL](../repl.md) |

## The bottom drawer

The bottom-right slot is a **single drawer** shared by three contents, each with
its own global toggle. Pressing a second toggle swaps the content in place rather
than stacking:

| Drawer | Toggle | What it is |
|--------|--------|------------|
| [REPL](repl.md) | `Ctrl+J` | persistent Julia session; values, stdout, and figures stream back as structured frames |
| [Terminal](terminal.md) | `Ctrl+T` | a local OS shell on the *frontend* machine — canonical use is SSHing out to backend hosts |
| [Monitor](monitor.md) | `Ctrl+M` | CPU / GPU / RAM history across your configured hosts |

Drawer toggles are **global** — they fire even when another pane holds focus.

## Focus and layout

| Action | Key |
|--------|-----|
| Move focus between panes (4-way, spatial) | `Ctrl+Arrow` |
| Navigate within the focused pane | plain `Arrow` |
| Maximize the focused pane | `Alt+=` |
| Restore the layout | `Esc` (only while a pane is maximized) |
| Cycle the active workspace | `Shift+Arrow` |
| Help overlay (build-current keymap) | `?` |

Plain arrows, `Ctrl+Arrow`, and `Shift+Arrow` are three disjoint roles, so they
never collide. The authoritative, build-current chord list is in
[Keybindings](../../ref/keybindings.md); when in doubt, `?` is the source of truth.
