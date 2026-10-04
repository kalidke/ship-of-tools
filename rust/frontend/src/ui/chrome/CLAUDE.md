# rust/frontend/src/ui/chrome: the pane chrome (fe-ui)

Focus and the panes it names, the status line, the nav spill overlay, the colour and flash helpers, the geometry of
the wireframe that frames the panes, and the frame's chrome draw: what is snapshotted (`draw_chrome`), what the ratatui
closure paints from it (`ChromeView`) and how the result becomes pixels (`pixels.rs`). Part of fe-ui; charter:
rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: declares the files and re-exports their items to the window code in `ui/`.
- `panes.rs`: `PaneFocus`, `DrawerContent`, `SpatialDir`, `PaneRects`, `maximize_slot`, and `State::set_focus`.
- `layout.rs`: pane geometry for a layout preset (`LayoutGeom`) and `draw_wireframe`, which paints the box-drawing frame.
- `status.rs`: status wrapping, the nav pane's pinned rows, the clock, battery and version labels, pane titles.
- `spill.rs`: `NavSpillSeg`, `nav_spill_take` and cell-width truncation for the nav spill overlay.
- `theme.rs`: RGB scaling, the contrast levers and the status-change flash.
- `strip/`: the session strip at the bottom: its items, the ships drawn on the band, and the band's rows and text.
- `replies.rs`: the status-line fields set by the active host's Connected event
- `draw.rs`: `State::draw_chrome`, which snapshots the chrome's inputs and runs the ratatui draw, and the snapshot
  pieces it calls.
- `pixels.rs`: the chrome's pixel layer: `State::project_chrome` (text lines and border quads) and
  `State::prepare_overlays`.
- `nav_body.rs`: `ChromeView::nav_body`: the nav pane's body inside the draw, with its row colours and spill
  segments.
- `view.rs`: `ChromeView`, the view the draw closure reads and writes, and its painting methods; `NavRow`.

## Start here
`panes.rs` for who has focus and which slot is shown; `status.rs` for what the bottom line and the nav pane's pinned
rows say; `layout.rs` for where the panes sit; `draw_chrome` in draw.rs for what one frame's chrome draw reads, then
`ChromeView::paint` in view.rs for what it paints.

## Rules
- `set_focus` is the only write of `focus` (test `focus_written_only_by_set_focus`, ui/scan_tests.rs).
- Leaving the nav pane dismisses the quit prompt (`set_focus` via `quit_prompt_on_focus`).
- While a leave line shows, nothing is maximized (`maximize_slot`).
- The nav pane's pinned rows are kept whole or dropped whole (`nav_pinned_rows`).
- The chrome draw reads only its `ChromeView`, the ratatui frame and the tree rows, and writes only the view's outs
  (`ChromeView::paint`).
