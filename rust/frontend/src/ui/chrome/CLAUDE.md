# rust/frontend/src/ui/chrome: the pane chrome (fe-ui)

Focus and the panes it names, the status line, the nav spill overlay, the colour and flash helpers, and the geometry of
the wireframe that frames the panes. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: declares the files and re-exports their items to the window code in `ui/`.
- `panes.rs`: `PaneFocus`, `DrawerContent`, `SpatialDir`, `PaneRects`, `maximize_slot`, and `State::set_focus`.
- `layout.rs`: pane geometry for a layout preset (`LayoutGeom`) and `draw_wireframe`, which paints the box-drawing frame.
- `status.rs`: status wrapping, the nav pane's pinned rows, the clock, battery and version labels, pane titles.
- `spill.rs`: `NavSpillSeg`, `nav_spill_take` and cell-width truncation for the nav spill overlay.
- `theme.rs`: RGB scaling, the contrast levers and the status-change flash.
- `strip/`: the session strip at the bottom: its items, the ships drawn on the band, and the band's rows and text.

## Start here
`panes.rs` for who has focus and which slot is shown; `status.rs` for what the bottom line and the nav pane's pinned
rows say; `layout.rs` for where the panes sit.

## Rules
- `set_focus` is the only write of `focus` (test `focus_written_only_by_set_focus`, ui/scan_tests.rs).
- Leaving the nav pane dismisses the quit prompt (`set_focus` via `quit_prompt_on_focus`).
- While a leave line shows, nothing is maximized (`maximize_slot`).
- The nav pane's pinned rows are kept whole or dropped whole (`nav_pinned_rows`).
