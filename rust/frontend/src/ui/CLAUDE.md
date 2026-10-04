# rust/frontend/src/ui: fe-ui, the window (charter)

## Idea
One UI thread owns all view state and every other thread reaches it only by waking the window; ratatui computes cells
and one wgpu pass draws every pixel.

## Owns
`State` and, for now, all of the window's code in mod.rs: the winit loop (`App`), drawing, input, the navigation trees,
the session view and agent pane, the drawers and the preview pane. Later units move each concept into a folder here.

## Promises
- Transport events are applied only on the UI thread (`State::drain_events`).
- A reply counts only if its generation, host and workspace are current (`reply_is_current`).
- Workspace maps key on (host, id) (`WsKey`).

## Connections
main.rs builds `App` through the alias `use ui as gpu;`. net/transport.rs sends `(HostKey, IncomingEvt)` in, takes
`OutgoingReq` out, and reads `crate::gpu::frontend_identity`.

## Folders
None yet.

## Files
- `mod.rs`: `State`, `App` and the rest of the window's code (over 800 lines under standing exemption E11).
- `scan_tests.rs`: the crate's own source for the tests that scan it.

## Start here
`State` in mod.rs for view state; `impl ApplicationHandler for App` for the event loop.
