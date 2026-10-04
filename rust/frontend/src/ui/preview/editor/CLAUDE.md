# rust/frontend/src/ui/preview/editor: in-pane editing (fe-ui)

The text buffer behind the preview pane's editor. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: declares `buffer`.
- `buffer.rs`: `EditBuffer`, a UTF-8 body with a byte cursor, edits and undo.

## Start here
`EditBuffer` in buffer.rs.

## Rules
- The cursor is a byte index on a char boundary (`EditBuffer::prev_boundary`, `next_boundary`).
- A typed run is one undo step and a cursor move ends it (test `cursor_move_breaks_the_undo_run`).
