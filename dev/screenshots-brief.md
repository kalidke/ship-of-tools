# Screenshot capture brief (for the win-fe FE agent)

Goal: capture screenshots for the Ship of Tools docs. Use the **`selfie`** skill to grab
the live FE window in each state, then save each PNG into
`/home/user/projects/sot-docs/docs/src/assets/<file>` (or
hand them to **@sot-docs**, who will place + embed + caption them).

For each shot: navigate the FE as described, let it settle, run `selfie`, crop to
the noted pane(s), save to the target filename.

| # | Target file | Navigate to | Capture |
|---|-------------|-------------|---------|
| 1 | `01-overview.png` | A real project loaded, all four panes populated (nav tree + rich preview + REPL + agent/LLM pane). The "this is Ship of Tools" hero. | full window |
| 2 | `02-files-nav.png` | Files mode (`f`); a directory expanded, a file cursored. | full window (or nav + preview) |
| 3 | `03-preview.png` | Cursor a rich file — a plot/PNG, rendered markdown, or a PDF page. | preview pane |
| 4 | `04-repl-plot.png` | REPL drawer (`Ctrl+J`); run a CairoMakie plot so it renders inline. | REPL pane incl. the inline figure |
| 5 | `05-modules.png` | Modules mode (`m`); drill modules → functions → methods, method source in preview. | full window |
| 6 | `06-sessions-states.png` | Sessions mode (`s`) with live agents visible — capture while the network shows a mix of **idle/working/blocked/waiting** (it does right now). | the Sessions view (show the colored states) |
| 7 | `07-monitor.png` | Monitor drawer (`Ctrl+M`) — CPU/GPU/mem across hosts. | the monitor drawer |

Notes:
- **Shot 6 is the money shot** for the agent-status story — grab it while sessions
  are genuinely in varied states (idle = gray, working, blocked = red,
  waiting = yellow, done).
- Prefer a real, recognizable project loaded (e.g. Ship of Tools itself).
- Bigger is better for docs; native window resolution is fine.
- When done, message **@sot-docs** the list of saved files (or confirm they are
  in `docs/src/assets/`).
