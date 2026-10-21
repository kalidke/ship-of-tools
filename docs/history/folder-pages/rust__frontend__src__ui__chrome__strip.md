# rust/frontend/src/ui/chrome/strip: the session strip (fe-ui)

The bottom band under the panes: one row of session names, grouped into ships (a brand wheel at each bow, a hull and
stern around the sessions of one host), with the host's name set into the waterline below. Everything here but draw.rs is
constants and pure functions of cell sizes and labels, so the geometry is unit-tested without a `State`; draw.rs is the
strip's frame, in `State` methods. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: declares the files; re-exports `band`, `hull` and `items` to the window code in `ui/`. `draw` is only
  declared.
- `items.rs`: `StripItem`, label truncation and badging, item widths, cursor positions, gaps and divider offsets.
- `hull.rs`: `StripMark` and its kinds, the hull's constants and vertical placement, scroll culling, box-name ink.
- `band.rs`: the band's reserved rows, the scroll target and animation constants, and `session_strip_lines`.
- `items_tests.rs`: tests of items, labels, widths and ship spans.
- `hull_tests.rs`: tests of the marks, hull geometry, waterline and box names.
- `band_tests.rs`: tests of the band's rows, the scroll target and the text lines.
- `draw.rs`: `State::draw_session_strip` and its pieces: the labels, the name lines, the ships and the scroll ease, for
  one frame.

## Start here
`strip_items` in items.rs for what the strip holds and where each item sits; `session_strip_lines` in band.rs for the
text drawn on it; `draw_session_strip` in draw.rs for how a frame lays the strip out; the hull constants at the top of
hull.rs for any change to the band's height.

## Rules
- The strip's rows come off the chrome grid once: `strip_reserved_rows`, called by `cell_grid_for`.
- The band never touches the grid's last row (test `strip_band_never_touches_the_grids_last_row`).
- A box's name is never painted in the active session's colour (`box_name_rgb`).
- One string is measured and drawn for a session: `strip_label` feeds the widths, the hull and the lines alike.
