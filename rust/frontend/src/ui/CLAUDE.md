# rust/frontend/src/ui: fe-ui, the window (charter)

## Idea
One UI thread owns all view state and every other thread reaches it only by waking the window; ratatui computes cells
and one wgpu pass draws every pixel.

## Owns
`State` and, for now, all of the window's code in mod.rs but the navigation trees (nav/): drawing, input,
the session view and agent pane, the drawers and the preview pane. Later units move each concept into a folder here.

## Promises
- Transport events are applied only on the UI thread (`State::drain_events`).
- A reply counts only if its generation, host and workspace are current (`reply_is_current`).
- Workspace maps key on (host, id) (`WsKey`).

## Connections
main.rs builds `App` through the alias `use ui as gpu;`. net/transport.rs sends `(HostKey, IncomingEvt)` in, takes
`OutgoingReq` out, and reads `crate::gpu::frontend_identity`. connections.rs (fe-net's data: which connection a request
goes to) and page_proxy.rs (the pages subsystem's window half) read `State`'s private fields, so the window holds them
until `State` is split.

## Folders
- `agent_pane/`: the agent pane's screen choice, attach client with warm pool, and input.
- `app/`: the winit application (`App`), the quit prompt and exit path, and the event loop's callbacks.
- `chrome/`: the pane chrome: focus and panes, the status line, nav spill, colours and the wireframe.
- `control/`: agent control of the window: the fe.command route, its dispatch, the nav envelope and the file channel.
- `drawer/`: the bottom drawer and its three tenants: the REPL, a Terminal and the Monitor.
- `input/`: the key-binding catalog, contextual help and clipboard paste.
- `nav/`: the navigation pane's trees (CLAUDE.md there).
- `persist/`: the window's settings, config discovery and resume state.
- `preview/`: the preview pane (image, markdown and editor subfolders).

## Files
- `agent_pane/`: the agent pane (its own page).
- `app/`: the winit application (`App`), the quit prompt and exit path, and the event loop's callbacks.
- `chrome/`: the pane chrome: focus and panes, the status line, nav spill, colours and the wireframe.
- `control/`: the agent control surface, with its own page.
- `drawer/`: the bottom drawer: the REPL log and lines, the Terminal's pty and vt100 helpers, the Monitor's view.
- `input/`: the key-binding catalog, contextual help and clipboard paste.
- `nav/`: the navigation pane's trees: mode and tree store, tree view, Modules, Sessions and Hosts trees.
- `persist/`: the window's settings, config discovery and resume state.
- `preview/`: the preview pane, with its image, markdown and editor subfolders.
- `mod.rs`: `State` and the rest of the window's code (over 800 lines under standing exemption E11).
- `connections.rs`: the window's view of its connection set: which connection a request goes to, and the per-host names (`send`, `send_to`, `default_host`, `ordered_hosts`).
- `page_proxy.rs`: arming a local listener so a remote daemon's page opens (`ensure_proxy_for_url`); the pages subsystem's window half.
- `scan_tests.rs`: the crate's own source for the tests that scan it.

## Start here
`State` in mod.rs for view state; `app/handler.rs` for the event loop.
