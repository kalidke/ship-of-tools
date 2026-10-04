# rust/frontend/src/ui/app: the winit application (fe-ui)

The window's application shell: `App` holds the window's `State` and its startup inputs, and the winit callbacks run
the event loop against it. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md. ADR 0050 describes the exit and
hand-over it serves.

## Files
- `mod.rs`: `App` and its constructor; `FRAME_BUDGET` and `NAV_FIRE_DEBOUNCE`, the frame pacing constants.
- `exit.rs`: the Ctrl+Q prompt's key table, `request_quit`, `leave` and `finish_exit`, and `redraw_exits`.
- `handler.rs`: `impl ApplicationHandler for App`: `resumed`, `window_event`, `about_to_wait`, `new_events`.

## Start here
`window_event` in handler.rs for any input or redraw change; `about_to_wait` for wake-up scheduling and the
`[display] fullscreen_vsync_pin` setting; `request_quit` in exit.rs for how the window closes.

## Rules
- Every user quit goes through `request_quit`: `exit_intent` asks on Ctrl+Q and leaves at once on the close button.
- `leave` never ends the drawer's session and sets `should_exit` before it polls (test `leave_never_ends_the_drawer`).
- A leaving window exits from `about_to_wait`'s poll once the acks are in (`redraw_exits`).
- A harness instance (`ephemeral`) starts neither watcher thread (`resumed`).
- Frames are capped at `FRAME_BUDGET` (`window_event`'s redraw arm and `about_to_wait`).
