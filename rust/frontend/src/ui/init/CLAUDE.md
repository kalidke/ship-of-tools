# rust/frontend/src/ui/init: building the window's state at launch (fe-ui)

`State::new` runs once, when the event loop first hands the app its window: it reads the resume file and settings,
creates the window and its GPU surface, builds the startup content and the `State` value, then applies the launch
flags. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: `State::new` and the launch steps it calls, in order.
- `fields.rs`: `State::from_parts`, the one struct literal that gives every `State` field its first value.

## Start here
`State::new` in mod.rs: one call per launch step, in the order they run. A new `State` field gets its first value in
`State::from_parts` in fields.rs.

## Rules
- Launch work runs in the order `State::new` calls it: the resume file and settings load before the window exists,
  the GPU surface (render/surface.rs) before any text or quad, and the launch flags after `State` exists.
- A field whose first value needs logic gets an `initial_<field>` function in fields.rs, called at the field's place in
  the literal, so the literal still evaluates in field order.
- Values built before the literal reach it inside named structs (`StartupParts` and the groups it carries), bound by
  field name at both ends, never by tuple position.
- Harness runs (`--capture`, `--ephemeral`) restore no persisted workspace, nav cursor or zoom
  (`initial_active_workspace_id`, `initial_pending_resume_nav`, `apply_startup_font_scale`).
- `State::from_parts` is the one function here over 100 lines: one line per `State` field, under 300 until `State`
  is split into parts.
