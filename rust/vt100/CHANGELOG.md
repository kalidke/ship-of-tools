# Provenance

This is a vendored fork of **vt100-ctt 0.17.1**
(`ChrisTitusTech/vt100-rust`, itself a fork of `doy/vt100-rust`), MIT.
Upstream's own changelog lives with those projects; it is not reproduced
here, because most of what it documents is API this fork has removed and a
changelog describing deleted functions is worse than no changelog.

## What this fork changes

**Added — the reason the fork exists.** `Screen::checkpoint` and
`Parser::restore_screen`: an exact, versioned, binary hand-off of terminal
state covering both grids, alternate-screen identity, the current and saved
cursor and attributes, the scroll region, origin mode, per-row wrap flags,
and the input modes. See `src/checkpoint.rs` for the format and
`docs/adr/0041-fe-local-capsules-windows.md` for why it exists: a capsule
outlives the frontend that renders it, and the frontend that attaches must
be handed the state rather than a reconstruction of it.

**Removed — what the addition replaces.** The escape-sequence *writing*
stack: `Screen::contents_formatted`, `contents_diff`, `rows_formatted`,
`rows_diff`, `state_formatted`, `state_diff`, `input_mode_formatted`,
`input_mode_diff`, `attributes_formatted`, `cursor_state_formatted`, the
`term` module implementing them, and the grid/row/attribute writers behind
it. ADR 0041 rejects that approach outright: replayed escape sequences
cannot express the inactive grid or alternate-screen identity. Also removed:
the `tui-term` feature and its ratatui glue, which the only consumer had
already switched off.

**Changed — one parser behavior, and only one.** `Row::resize` now clears a
wide character's leading cell when shrinking cuts off its continuation,
which `Row::truncate` already did. Without it, shrinking a terminal through
a wide glyph left a lead with no continuation, and drawing over that cell
reached for the missing half and panicked. (`Row::truncate` also no longer
indexes past the end when resized to zero.)

Nothing else about parsing differs from the release this vendors.
