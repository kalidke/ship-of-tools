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
- Preview and concept replies require the current generation, host and workspace. Result-owned tree replies also require the currently listed canonical row and issuing result/attempt before active or parked tree installation and reveal effects; ordinary nav replies retain their tree-key routing.
- Workspace view maps use WsKey = (host, normalized slug); strip keys use the listed (host, slug). Canonical ids are resolved through workspace.list before command effects.
- Final window teardown has one shared three-second OS-clock process deadline, independent of the UI and transport runtime; it starts at the terminal decision, after any daemon acknowledgement and notice presentation.
- `State::set_focus` (chrome/panes.rs) is the one write of `focus`; scan_tests.rs (`focus_written_only_by_set_focus`)
  fails any other.
- Input reaches only the selected row's client or is counted: `State::send_pane_input` (agent_pane/input.rs) bumps
  `pane_inputs_discarded` when there is no live client.
- Text over 512 KiB, or binary, is summarized, never shaped: `PREVIEW_TEXT_CAP` in preview/pane.rs.
- Harness instances (`ephemeral`: `--ephemeral`, any `--capture`) write no shared file and arm no watcher:
  `persist_resume_state` (persist/mod.rs), `maybe_write_fe_state` and the command watcher (control/file_channel.rs),
  `resumed` (app/handler.rs); a relaunch command is refused for them (control/dispatch.rs).
- A presentation receipt requires the current live attached client's checkpoint, a nonempty painted pane and a known request origin, and is emitted once only after frame presentation.
- Showing a result never steals the view: `route_fe_command` (control/command.rs) honours force-show only for a
  command addressed to this frontend, and a broadcast `relaunch` is refused.
- Help is one chord: `help.toggle` = `Primary+?` and `drawer.help` = `F1` (input/keybindings.rs). There is no bare `?`
  binding and no "press ?" wording in hints; a bare-text chord never fires where the pane consumes typed text
  (`Chord::is_bare_text`). A `keybindings.toml` entry replaces that action's default chords. If Ctrl+? fails on a
  layout, fix the chord normalization, never add a bare-key fallback.
- `[display] fullscreen_vsync_pin` defaults false and is a per-box choice (persist/settings.rs; applied in
  `about_to_wait`, app/handler.rs, and announced in input/global_keys.rs). Never add an always-redraw path that ignores it.

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: `fe.command.send`, `fe.command`,
`agents/sot-fe/sot-fe-request.sh`, `sot_ui`, `agent.message`, `agents/sot-fe/sot-nav.sh`, `comm-relay.sh send --all`.
Uses: `DaemonLaneEndpoint`, `fe.lease`, `fe.leaving`, `fe.notice_seen`, `rust/frontend/src/lease.rs`,
`workspace.create`, `workspace.destroy`, `workspace.list`, `workspace.reauth`, `pty.input`, `pty.screen`,
`workspace.changed`, `FeAttachClient`, `rust/backend/src/rows/run/headless.rs`,
`rust/frontend/src/ui/agent_pane/attach.rs`, `sot_state_dir`, `sot_config_dir`, `host_name`, `state_dir_hash`,
`spawn_registry_poll`, `tree.root`, `tree.children`, `directory.list`, `nav.toggle_hidden`, `preview.get`,
`preview.set_scale`, `image.crop`, `concept.read`, `concept.write`, `concept.list`, `file.read`, `file.write`,
`file.delete`, `file.download`, `file.upload`, `dir.create`, `preview.changed`, `repl.eval`, `repl.run_file`,
`repl.interrupt`, `repl.execute`, `kernel.request`, `math.render`, `pluto.open`, `monitor.subscribe`,
`monitor.unsubscribe`, `monitor.history`, `repl.frame`, `monitor.tick`, `video.open`, `docs.open`, `quarto.open`,
`proxy.connect`, `ensure_proxy_for_url`, `pipe_one`, `loopback_port_from_url`, `rust/protocol/src/page_url.rs`,
`OutgoingReq`, `IncomingEvt`, `HostTable`, `lane_dial`, `ResolvedDial`, `--socket`, `--dial`, `--relaunched`,
`relaunch.request`, `spawn_watcher`, `rust/frontend/src/relaunch.rs`, sot_protocol::annotation::split_frontmatter, sot_protocol::annotation::synced_against, sot_protocol::physical_scale::PhysicalScale, sot_protocol::physical_scale::parse_physical_scale, sot_protocol::video_path::video_mime, lease_notice, handle_fe_command_send relay diagnostics.

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
- `events.rs`: drain_events routes each IncomingEvt variant to its owner, forwarding result-tree attempt identities unchanged to the navigation reply handler; event-service scheduling remains owned by the app.
- `connections.rs`: the window's view of its connection set: `send`, `send_to`, `default_host`, `ordered_hosts`.
- `page_proxy.rs`: arming a local listener, at a port of the window's own, so a remote daemon's page opens, and the
  URL that opens it (`ensure_proxy_for_url`, `bind_proxy_listener`).
- `scan_tests.rs`: the remaining inherited focus-write and ROI-paste source scans; the leave contract is tested behaviorally by the inline begin_leave tests in app/exit.rs.
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
