# rust/frontend/src/ui: fe-ui, the window (charter)

## Idea
One UI thread owns all view state and every other thread reaches it only by waking the window; ratatui computes cells
and one wgpu pass draws every pixel. The window is one `State` (mod.rs) driven by one winit loop; each concept below
is a folder whose `impl State` blocks and free functions reach the shared fields through `use super::*`.

## Owns
- The winit loop, frame pacing and the exit path: `app/`.
- The render stack (glyph text, the cell backend, textured quads, the surface and capture) and the frame's one render
  pass: `render/`, `render/pass/`.
- Chrome, key bindings, help and paste: `chrome/` (panes, status line, nav spill, strip), `input/`.
- The navigation trees: `nav/`. The session view and its switch: `session/`. The agent pane: `agent_pane/`.
- The REPL, Terminal and Monitor drawers: `drawer/`. The preview pane, images, markdown and the editor: `preview/`.
- The agent control surface (`fe.command`, the command directory, `fe-state.json`, the `sot_ui` envelope): `control/`.
- UI persistence (settings.toml, keybindings.toml discovery, the per-host resume file): `persist/`. Downloads to disk:
  `nav/files/`.
- Building the state at launch: `init/`.

## Promises
- Daemon events are applied only on the UI thread, at the top of each frame: `State::drain_events` (events.rs) is called
  from `State::frame_upkeep` (app/frame.rs). It only routes: one arm per `IncomingEvt` variant, each calling
  `on_<variant>` in its owner's `replies.rs`, and no wildcard arm, so a new variant does not compile until routed.
- Other threads reach the window only by `Window::request_redraw` (the relaunch watcher, the `FeAttachClient` wake
  closure in agent_pane/attach.rs, page_proxy.rs).
- An action resolves only if `help::Context::allows` it (input/keypress.rs passes it to the resolver), so help and
  dispatch cannot disagree.
- A reply counts only if its generation, host and workspace are current: `reply_is_current` (preview/fetch.rs) for
  the preview and concept slots; nav replies go into the tree their key names (`State::swap_active_tree`).
- Workspace maps key on `WsKey` = (host, id) (mod.rs; keys built in session/workspace_key.rs), never a bare slug.
- `State::set_focus` (chrome/panes.rs) is the one write of `focus`; scan_tests.rs (`focus_written_only_by_set_focus`)
  fails any other.
- Input reaches only the selected row's client or is counted: `State::send_pane_input` (agent_pane/input.rs) bumps
  `pane_inputs_discarded` when there is no live client.
- Text over 512 KiB, or binary, is summarized, never shaped: `PREVIEW_TEXT_CAP` in preview/pane.rs.
- Harness instances (`ephemeral`: `--ephemeral`, any `--capture`) write no shared file and arm no watcher:
  `persist_resume_state` (persist/mod.rs), `maybe_write_fe_state` and the command watcher (control/file_channel.rs),
  `resumed` (app/handler.rs); a relaunch command is refused for them (control/dispatch.rs).
- Showing a result never steals the view: `route_fe_command` (control/command.rs) honours force-show only for a
  command addressed to this frontend, and a broadcast `relaunch` is refused.
- Help is one chord: `help.toggle` = `Primary+?` and `drawer.help` = `F1` (input/keybindings.rs). There is no bare `?`
  binding and no "press ?" wording in hints; a bare-text chord never fires where the pane consumes typed text
  (`Chord::is_bare_text`). A `keybindings.toml` entry replaces that action's default chords. If Ctrl+? fails on a
  layout, fix the chord normalization, never add a bare-key fallback.
- `[display] fullscreen_vsync_pin` defaults false and is a per-box choice (persist/settings.rs; applied in
  `about_to_wait`, app/handler.rs, and announced in input/global_keys.rs). Never add an always-redraw path that ignores it.

## Connections
- In from fe-net: `(HostKey, IncomingEvt)` on one std mpsc fan-in into events.rs; `HostTable` (`State::hosts`) and
  `frontend_identity` live in rust/frontend/src/net/ (ui re-exports the identity).
- Out through `State::send` and `send_to` (connections.rs): `OutgoingReq` on the per-host sender, routed by host.
- Lifecycle (rust/frontend/src/lease.rs): `exit_intent` decides a quit, `State::request_quit` and `State::leave`
  (app/exit.rs) call `Leases::leave_all`, and the process exits once each held lease's daemon acks or its wait ends.
- Pages: page_proxy.rs `ensure_proxy_for_url` arms a local listener through the manager in rust/frontend/src/pages.rs.
- Distribution: rust/frontend/src/relaunch.rs `spawn_watcher` sets the exit flag (75 relaunch, 76 converge) and wakes
  the window.
- Capsule: `FeAttachClient` (sot_log) in agent_pane/attach.rs, over the lane the control connection resolved.
- main.rs resolves the dial set, builds the evt channel and one outgoing channel per host, then calls
  `gpu::App::new(...)` (`use ui as gpu;` in main.rs) and `event_loop.run_app`; the window and `State` come later, in
  `resumed` (app/handler.rs) through `State::new` (init/). Transport tasks start there (`net::hosts::spawn_transports`).

## Folders
- `app/`: the winit application (`App`), event-loop callbacks, one frame (`frame.rs`) and the quit prompt and exit.
- `render/`: glyph text, the ratatui cell backend, textured quads, the surface and the capture readback.
- `render/pass/`: the frame's render pass, one `State` method per section, called in order by `State::redraw`.
- `chrome/`: panes and focus, the status line, nav spill, colours, the wireframe and the frame's chrome draw.
- `chrome/strip/`: the session strip band under the panes.
- `input/`: the action catalog, key and mouse routing, help and clipboard paste.
- `nav/`: the navigation pane's trees (mode, store, Modules, Sessions, Hosts).
- `nav/files/`: Files mode's prompts, reveal walk, transfers and re-lists.
- `session/`: which row the window is on: the workspace lists, snapshots, the switch, picker, presence, badge floor.
- `agent_pane/`: the agent pane's screen choice, attach client with warm pool, and input.
- `drawer/`: the bottom drawer and its tenants.
- `drawer/repl/`: the REPL tenant (log, submit, history, display lines).
- `drawer/terminal/`: the Terminal tenant (local pty, attach backend, vt100 helpers).
- `preview/`: the preview pane (fetch, replies, layout, concept, open).
- `preview/image/`: bitmap previews as quads: figures, overlays, view, ROI.
- `preview/markdown/`: markdown and source text shaped into cosmic-text buffers.
- `preview/editor/`: the in-pane edit buffer.
- `control/`: the fe.command route, its dispatch, the nav envelope and the file channel.
- `persist/`: settings, config discovery and the resume snapshot.
- `init/`: `State::new` and the field defaults at launch.
- Outside ui/, fe-ui also owns rust/frontend/src/main.rs (argument parsing, the connection set, the `App` build) and
  cli.rs (the `Cli` flags).

## Files
- `mod.rs`: `State` and the module declarations (over 800 lines under standing exemption E11).
- `events.rs`: `drain_events`, which applies each daemon event (`IncomingEvt`) to the window by variant.
- `connections.rs`: the window's view of its connection set: `send`, `send_to`, `default_host`, `ordered_hosts`.
- `page_proxy.rs`: arming a local listener so a remote daemon's page opens (`ensure_proxy_for_url`).
- `scan_tests.rs`: the crate's own source for the tests that scan it (focus writes, leave, paste).
- `agent_pane/`: the agent pane (its own page).
- `app/`: the winit application and exit path (its own page).
- `chrome/`: the pane chrome (its own page).
- `control/`: agent control of the window (its own page).
- `drawer/`: the bottom drawer (its own page).
- `init/`: building the window's state at launch (its own page).
- `input/`: key bindings, help and paste (its own page).
- `nav/`: the navigation pane's trees (its own page).
- `persist/`: settings, discovery and resume state (its own page).
- `preview/`: the preview pane (its own page).
- `render/`: the window's pixels (its own page).
- `session/`: which session row the window is on (its own page).

## Start here
`State` in mod.rs for view state; `app/handler.rs` for the event loop; `app/frame.rs` for one frame; `events.rs` for a
wire reply (then the `on_<variant>` it names); `input/keypress.rs` for a key. Each folder's page says its own rules.
