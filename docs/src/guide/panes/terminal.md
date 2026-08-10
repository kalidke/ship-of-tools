# The Terminal Drawer

*Bottom drawer — `Ctrl+T`.* The Terminal is a **local OS shell on the frontend
machine**. Its canonical use is SSHing outward to backend hosts, and because it is
local — **not** proxied through the daemon — it works even when the backend is
unreachable. It shares the bottom drawer slot with the [REPL](repl.md) (`Ctrl+J`)
and the [Monitor](monitor.md) (`Ctrl+M`): each key toggles its own content, and
pressing another swaps it in place.

| Key | Drawer closed | Showing this content | Showing other content |
|-----|---------------|----------------------|-----------------------|
| `Ctrl+T` | → Terminal | → closed | → Terminal |
| `Ctrl+J` | → REPL | → closed | → REPL |

## Scrollback

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

## The dev session lives here

When Ship of Tools is developed on itself, the dev `claude` session runs **inside
this Terminal drawer**. Two consequences worth internalizing:

- **Never kill the frontend process to restart it** — that kills your own session
  along with it. Use the self-relaunch loop (build → sentinel → exit-75 → re-stage
  → respawn) instead.
- On a self-relaunch, the frontend reopens straight into this drawer and runs the
  configured `[terminal] resume_command` (`claude --continue …`) as its first
  command, so the session resumes from its own store without prompts.

Both are covered in [Running & Relaunch](../../start/running.md).

## See also

- [Running & Relaunch](../../start/running.md) — launching, reconnecting (`F5`), and the self-relaunch loop.
- [The REPL](repl.md) and [The Monitor](monitor.md) — the other two drawer contents.
- [Keybindings](../../ref/keybindings.md) — the global drawer toggles.
