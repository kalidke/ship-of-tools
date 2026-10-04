# rust/frontend/src/ui/init: building the window's state at launch (fe-ui)

`State::new` runs once, when the event loop first hands the app its window: it reads the resume file and settings,
creates the window and its GPU surface, builds the startup content and the `State` value, then applies the launch
flags. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: `State::new`, the launch sequence.

## Start here
`State::new` in mod.rs, top to bottom: its work runs in the order written.

## Rules
- Launch work runs in the order `State::new` does it: the resume file and settings load before the window exists,
  the GPU surface before any text or quad, and the launch flags after `State` exists.
- Harness runs (`--capture`, `--ephemeral`) restore no persisted workspace, nav cursor or zoom.
