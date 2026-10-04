# rust/frontend/src/ui/drawer: the bottom drawer (fe-ui)

One drawer sits under the window's panes and shows one tenant at a time: the Julia REPL, a Terminal or the server
Monitor (ADR 0041). Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: declares the tenants' folders and files.
- `keys.rs`: What a key does in the drawer: clear and paste, then the Terminal's pty or the REPL's scroll, history and input.
- `monitor.rs`: the Monitor tenant's state (a ring of samples per host) and its SVG chart (ADR 0020), and
  `State::layout_drawer_px`, which sizes the drawer's rects and rasterises the chart.
- `repl/`: the REPL tenant, its eval log and the log's display lines.
- `terminal/`: the Terminal tenant, the local pty and the vt100 helpers.

## Start here
`MonitorView` in monitor.rs for the Monitor's data and chart; repl/ and terminal/ hold the other two tenants.

## Rules
- A stale tick flags its host and appends nothing (`MonitorView::apply_tick`).
- Each host's ring holds at most `RING_CAP` samples (`HostBuf::push`).
