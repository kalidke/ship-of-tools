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
