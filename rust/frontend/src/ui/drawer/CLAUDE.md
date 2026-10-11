# rust/frontend/src/ui/drawer: the bottom drawer (fe-ui)

One drawer sits under the window's panes and shows one tenant at a time: the Julia REPL, a Terminal or the server
Monitor (ADR 0041). Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: declares the tenants' folders and files.
- `keys.rs`: What a key does in the drawer: clear and paste, then the Terminal's pty or the REPL's scroll, history and input.
- `monitor.rs`: the Monitor tenant's state (a ring of samples per host), its subscription and its SVG chart (ADR 0020),
  and `State::layout_drawer_px`, which sizes the drawer's rects and rasterises the chart.
- `repl/`: the REPL tenant, its eval log and the log's display lines.
- `terminal/`: the Terminal tenant, the local pty and the vt100 helpers.

## Start here
`MonitorView` in monitor.rs for the Monitor's data and chart; repl/ and terminal/ hold the other two tenants.

## Rules
- A stale tick flags its host and appends nothing (`MonitorView::apply_tick`).
- A host whose last reading arrived more than `GAP_S` (5 s) ago, or whose source the daemon reports down, is marked
  `*` with the shown reading's age (`HostBuf::mark`). The window's own clock decides.
- The chart on screen is redrawn whenever the readings change (`monitor_dirty`) or it was drawn for another size, text
  scale or set of marks (`MonitorView`'s `drawn`, cleared while the drawer shows another tenant), so a mark that ages
  with no tick, a resize and a return to the Monitor each show a true chart. The once-a-second idle repaint makes the
  check with no tick arriving (`layout_drawer_px`).
- Each host's ring holds at most `RING_CAP` samples (`HostBuf::push`).
- The window holds the Monitor subscription from the drawer's open to its close by the toggle
  (`MonitorView`'s `subscribed`), and only monitor.rs changes it: `sync_monitor_subscription` at the toggle,
  `hold_monitor_subscription` for `--start-monitor`. The daemon starts every connection unsubscribed, so each new
  connection to the monitor host is sent it again with the history prefill (`State::resubscribe_monitor`, called from
  `on_connected`).
