# rust/frontend/src/ui: fe-ui, the window (charter)

## Idea
One UI thread owns all view state and every other thread reaches it only by waking the window; ratatui computes cells
and one wgpu pass draws every pixel. The window's `State` (mod.rs) is driven by one winit loop. `session/row_directory.rs`
owns authoritative rows and identity; `session/row_presentation.rs` owns their presentation;
`session/result_ledger.rs` owns owed results and their attempts; `agent_pane::AgentPane` owns the selected agent pane.
`drawer/repl::ReplModel` owns REPL row state, submissions, history, routing and lifecycle;
`drawer/repl/presentation::ReplPresentation` owns window-wide REPL images, layout and scroll anchoring.
`input::FocusPrompts` owns window-wide focus, nav prompts, captured scale focus, stayed notice and quit hint.
The remaining concepts below still reach shared fields through `impl State` blocks and free functions using `use super::*`.

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
- Other threads reach the window only through `MainWake::wake` (app/mod.rs): the transports, the relaunch and
  fe-command watchers, the drawer terminal's reader and the `FeAttachClient` wake closures (agent_pane/attach.rs). `State`'s window is a `MainWindow`, which is `!Send`, so no other thread holds it, and `create_window` returns it
  directly: no bare `Arc<Window>` exists outside it and wgpu's surface; on macOS a `Window` call from
  another thread waits for the frame being drawn.
- An action resolves only if `help::Context::allows` it (input/keypress.rs passes it to the resolver), so help and
  dispatch cannot disagree.
- Preview and concept replies require the current generation, host and workspace. Result-owned tree replies also require the currently listed canonical row and issuing result/attempt before active or parked tree installation and reveal effects; ordinary nav replies retain their tree-key routing.
- Workspace view maps use WsKey = (host, normalized slug); strip keys use the listed (host, slug). `RowDirectory`
  privately owns authoritative host listings and canonical-id translation. Its strict resolver admits a command
  only for one listed row with a nonempty, unique canonical id on the specified dial host, and returns an owned
  `ResolvedWorkspace` carrying canonical identity, listed key, normalized view key and the stored attachment name.
  Default aliases and inert-anchor visibility retain their existing rules. Lifecycle hints use a separate tolerant
  lookup: the event host qualifies known ids, unknown hints pass through, and absent hints use the caller's existing
  default-slug fallback. No caller can mutate either directory map or its row slices.
- Pass 7a's one deliberate difference from `1bc65e054`: canonical-id translation covers every installed listing,
  including a synthetic host absent from `conns`, whereas the old translation was limited to `ordered_hosts`.
  Real listing events arrive only from the fixed connection set, so this difference is unreachable with real
  connections. Presentation projections still use `ordered_hosts`. Behavior test:
  `workspace_list::tests::canonical_translation_includes_a_host_outside_ordered_hosts`.
- `RowDirectory::prepare_replace` reports canonical identities retired by one host's authoritative listing without
  changing the installed rows; same-slug canonical replacement retires the old identity, while unchanged rows and
  kept defaults survive. `on_workspaces` reconciles owed results before committing the replacement, then rebuilds
  presentation projections, prunes warm attachments, updates declarations and rebuilds or parks the Sessions tree
  (with the existing capture-cycle step before the tree). Disconnect leaves the directory intact. Behavior tests:
  `row_directory::tests::{union_replace_leaves_the_other_hosts_list_intact,
  lifecycle_key_uses_the_event_host_and_keeps_legacy_fallbacks,
  authoritative_replacement_reports_only_retired_canonical_identities}` and
  `row_directory::listed_tests::{listed_resolution_keeps_each_host_and_key_space,
  ambiguous_empty_and_unknown_targets_are_refused}`. The native `pending_result_dies_with_its_canonical_row`
  fixture covers reconciliation effects and disconnect retention; its `kept_default_preserves_its_result_and_view`
  control checks the default row's own pending result, cached tree selection, preview source and REPL input through
  workspace switches. The native fixture runs on hosted Windows and macOS.
- `RowPresentation` privately owns labels, canonical-root fallbacks, state/stamp pairs, transition history,
  flashes, strip order and the active host's default. Projections use only `ordered_hosts` and listed host/slug keys;
  inert defaults retain metadata. None selections use hello metadata; unknown nondefaults get no root fallback.
  Stable host blocks rank blocked, badge, done, working, waiting, rest and pin the selection's previous slot.
  Known changes flash; missing rows retain history. Sessions/strip share the 600 ms fade. Destroy forgets only
  labels/roots; capture supports flashes without demo rows.
  The clock is sampled separately at each changed-row insertion after projection and at each explicit demo flash
  after capture seeding, preserving the timing of `1bc65e054`.
  Behavior tests: `workspace_list::tests` and `row_presentation::tests`.
- `ResultLedger` privately owns canonical owed results, the listed badge index, both checked serials and the
  hostless issuing reveal token. Replacements reset attempts; retries advance independently, and neither beginning
  an attempt, releasing reveal ownership nor abandoning an unsent attempt consumes the owed result. Admission
  requires the event dial host, active host, current listing, reveal ownership and both current serials. Cursor
  landing releases the marker and certifies the node; preview installation certifies only the matching generation
  and node after State's reply admission and source installation. Only the post-present operation acknowledges,
  with both certificates and matching displayed cursor/source nodes on the resolved row. Kept outcomes do nothing;
  removed outcomes invalidate the exact canonical identity. Prepared retirements reconcile only pending canonical
  identities before directory commitment; State applies returned reveal cleanup and badge refresh effects.
  The owner never takes State or exposes mutable storage. State retains navigation, preview clocks, sends,
  presentation ordering and persistence. Behavior tests: `result_ledger::tests`; the retained badge status test is
  `badge::tests::pending_nav_status_names_workspace_and_path`. The ten-fixture native registry also runs
  `replaced_result_rejects_old_tree_attempt_before_effects`, alongside the badge, retirement/disconnect and
  kept-default controls. Registration alone is not hosted execution evidence.
- `ReplModel` privately owns live and parked row logs, drafts, prompt modes, scroll and history walks, plus
  the window-wide saturating eval counter, interrupt memory, host-qualified eval routes, run-file display records
  and raw-slug lifecycle state. It lends immutable entries and returns owned send plans and effects; its operations
  never take State or expose mutable storage. ReplPresentation owns image caches, geometry and build anchors;
  State retains transport, status, browser/proxy work and persistence. REPL records remain runtime memory, with no new resume format.
- Eval submission consumes whitespace without advancing the counter or resetting history; real submissions register
  before send and trim to 255 before appending. Delayed demo submission replaces the draft without moving scroll,
  including whitespace that submits nothing. Failed sends close the visible entry with an error and retain the
  route. Registered replies use (event dial host, eval id) and the normalized sending-row key, even across switches;
  a recorded missing parked row drops output. Unknown owners keep the legacy live-log fallback. Acceptance leaves
  routing and flight state intact; terminal replies append to streamed output. Done consumes run-file display
  information and reports real elapsed time. Reconnect reopens only that host's named closed live/parked runs,
  appends the link-back line and reinstalls routing.
- Lifecycle and Browser are control frames, handled before eval mutation. State translates lifecycle hints through
  RowDirectory and Started hints through reply_ws_key; Started admits only existing live/parked destinations and
  skips duplicate ownership. Lifecycle refreshes the event host's listing. Presentation supplies nonempty listing
  catch-up; insertion retains unmentioned values but may overwrite mentioned ones, without freshness arbitration.
  Default migration preserves an existing destination record and rewrites only matching host-qualified routes;
  lifecycle retains raw slugs. Destroy forgets only the named parked record at State's existing cleanup point.
- Edits reset scroll to the tail without resetting a history walk; first Up saves the original draft and Down
  beyond newest restores it. Only in-flight entries are excluded: completed session-originated runs participate.
  Entry prompts keep their submission mode. Interrupt prepares every ordered live in-flight id, forces only the
  same remembered set, and remembers only successful sends. Interrupt memory stays window-wide through restore,
  clear and fresh runs. With no run, Ctrl+C clears draft and exits pkg mode. Page/wheel integer conversion and
  saturating fine-line scroll retain their distinct arithmetic; rendering supplies the final clamp.
- Ctrl+L clears log, scroll and ReplPresentation's image caches, retaining draft, prompt mode and history walk. A fresh file
  run clears log, scroll, mode and history, retaining draft and image caches until normal pruning. Every restore
  requests a build-anchor reset, including a first visit. Behavior proofs: `drawer::repl::model_tests`, including
  demo scroll preservation and Started-frame dispatch, and the moved `drawer::repl::log::tests` characterizations.
  The two existing native REPL fixtures still run through the event queue with allocated ids, alongside the
  kept-default draft control; hosted
  Windows/macOS execution remains required.
- `ReplPresentation<I = Quad>` privately owns decoded images, rebuilt image slots, the previous completed
  scrollback pixel rectangle, the half-open visible line window and the build anchor. Image and slot records are
  private too. State supplies the normalized row key, starting flag, immutable model entries and cell metrics;
  the owner never takes State or mutable model storage. Real base64/image decoding uses a narrow upload port;
  the same clipping and placement operation uses a narrow painter in production and portable recorder tests.
- Image identity is (normalized WsKey, eval id, frame index). P7f-image-alias names the old cross-row cache hit;
  its behavior witness is `images_follow_the_row_eval_and_frame_that_produced_them`, with the unqualified-identity
  mutation run first before accepting the correction. Cache refresh decodes missing images before pruning;
  successes are reused, failures retried and retention follows the active row's surviving eval ids, including
  every cached frame index of each survivor. There is no parked texture cache, LRU or new image budget.
- Chrome refreshes images and builds owned lines before ratatui, for every drawer tenant. Builds use the previous
  pixel width/height (zero initially, with caption fallback), preserving the one-frame resize lag. The owner fits
  figures to both dimensions without upscaling and replaces slots every build. Chrome computes capped multiline
  input and tail-relative clamped scroll through the owner even for other drawer tenants. State commits model
  scroll only after ratatui succeeds, preserves pane/terminal resize order, then commits the REPL pixel rectangle
  and window before Monitor layout. Image painting remains after Monitor and before media/text, clipped to the
  REPL scrollback with one-cell left inset and absolute slot/window offset; success restores the full-surface
  scissor and painter errors propagate. Presentation and result acknowledgments remain after frame presentation.
- Anchor geometry compares raw bits of width, height and both cell metrics, excluding rectangle position.
  Matching geometry compensates signed growth from the previous newest entry's start, searches numeric eval ids
  from the end and clamps to u16; tail zero follows output, vanished anchors leave scroll alone and reseed, and
  empty builds remove the anchor. Restore resets only the anchor on found and missing rows. Pixel geometry stays
  window-wide, outside row snapshots and persistence. Clear effects remove images/slots only; fresh file runs
  leave them until normal pruning. Portable behavior tests: `drawer::repl::presentation::tests` and
  `chrome::view::tests::repl_drawer_layout_uses_the_owner_window`; existing line and shaping controls remain.
  These tests establish owner decisions; native REPL/result/rendering and the pre-present pane fault still require
  the hosted Windows/macOS release gate before this rendering-owner pass lands.
- `State` applies REPL catch-up, migration on every applicable refresh, then status before attachments,
  declarations, capture cycling and tree rebuild. Switching snapshots before host/default changes and restores
  REPL independently of the general UI snapshot. Connected reopening still precedes session on_connected;
  destruction still reconciles the ledger before bounce/cache cleanup. The exhaustive router is unchanged.
- Final window teardown has one shared three-second OS-clock process deadline, independent of the UI and transport runtime; it starts at the terminal decision, after any daemon acknowledgement and notice presentation.
- `FocusPrompts` privately owns focus, nav prompts, captured scale focus, stayed notice and quit hint. It lends
  immutable observations and owned drafts, never takes State or exposes mutable storage. `State::set_focus` only
  delegates; moves preserve prompts/notices. State retains admission, validation, sends, pending markers and redraw.
  Create closes on send success; delete on either outcome. Scale success enables the bar, closes, arms save, then
  restores captured focus once; failures retain text/focus. Cancellation consumes saved focus even after leave
  removed its prompt. Quit alone refuses replacement. Begin leave clears only prompt/stayed; only successful lease
  stay reopens Yes for Keep or records Handover stayed. Text consumers keep a stayed dismissal key. Raw-key, picker,
  pointer presence/modal/focus/selection ordering and dialog precedence remain unchanged. Quit-open/refusal events
  retain the `sot::ui::app::exit` target and finite fields; the other moved events retain their original modules.
  Hints use bound labels, format text before sampling their clock, take no input and wake/expire at four seconds
  including equality. All fields are runtime only.
  Behavior tests replace both focus scans: `input::focus_prompts_tests`, `input::keypress::tests`,
  `nav::files::prompt::tests`, `app::{exit,dialog}::tests`, `app::handler::idle_tests`, `chrome::panes::tests`.
  They drive production continuations. Native window-close retains its scenarios and hosted gate requirement.
- `AgentPane` privately owns the host/attachment target, size, selection/drag, selected client, host-bounded warm
  pool, hold, dial reason, request/client clocks, warning count, feed, discarded-input count and composed
  `PanePresentation`. Its operations never take State or expose mutable clients/screens/storage. The narrow
  `PaneClient` port uses `FeAttachClient` in production and a scripted client in `agent_pane/owner_tests.rs`.
  State applies owned effects for transport sends, asynchronous retirement, logging, redraw, clipboard and persistence;
  it keeps host/dial configuration, pointer geometry/residue, create origins and multi-owner event ordering.
- Switch compares the whole host/attachment pair, captures/parks the departing client and sets Pending before sending;
  only a successful open send commits the target. Direct replies require that pair, the active host and an empty slot.
  Exact warm reuse precedes dial configuration; pruning uses committed host-local stored attachment names, including
  same-name canonical replacements. Unresolved dials stay retryable; missing/unusable configuration and matching
  open failures retain reasons. Warnings count changed failure-status edges and reset on each installed client.
  The coordinator logs a prepared attach before warm resume and before sampling the installed client's start clock;
  the request clock keeps its earlier origin. Direct-reply admission already guarantees an empty active slot.
- Input reaches only the selected pane client or is counted once per call; Pending input is discarded without replay.
  Held pixels never receive input. Copy uses client-or-blank and clears selection only after clipboard success.
  Pending resize tracks geometry without sending; page/wheel routing preserves local scrollback, alternate-screen
  fallthrough and mouse tracking. Selection survives release, switches and resizing.
- Pane draw preserves presence/read-mark, pane pump, drawer pump, immutable screen borrow, draw, geometry/resize and
  painted-claim ordering. Only post-present completion can consume a live attached checkpoint's receipt; held pixels,
  empty area, stale requests and missing origins cannot report success. The existing `agent_pane/presentation_tests.rs`
  cases remain; `agent_pane/owner_tests.rs` characterizes the owner guarantees, including exact host-local eviction
  victims at an intermediate bound and ordering between attach logs and client clocks. The portable
  `input::mouse::tests::a_wheel_step_reports_presence_before_fractional_routing` drives the production wheel
  dispatcher through a narrow context port, including sub-row early returns. The native State fixture
  `a_wheel_step_reports_presence` replaces the wheel presence scan, including fractional steps and zero-area pointer
  mapping controls, alongside the previous nine fixtures. Native execution still requires hosted evidence.
- Text over 512 KiB, or binary, is summarized, never shaped: `PREVIEW_TEXT_CAP` in preview/pane.rs.
- The window draws text from outside it (a daemon's reply, a REPL's output, an agent's words, a file's name, a
  paste) in a cell run, a strip name or the Monitor's chart only through `cell_lines`, one row per line, or
  `cell_row`, its lines joined by ` ⏎ ` where only one row exists (chrome/status.rs), so none of them holds a colour
  code, a control character or a line or paragraph separator: ratatui drops a `\n` inside a span, the glyph layer
  draws a `\r` or a `\t` as a line break or a tab stop, cosmic-text panics on a second bidi paragraph in one line,
  and usvg rejects a whole chart for one (and for U+FFFE or U+FFFF, which the chart draws as U+FFFD). The preview's
  shaper gets the same line ends (`normalize_newlines`, preview/markdown/buffer.rs). Tests
  `untrusted_lines_stay_apart_in_the_chrome` (chrome/view.rs), `strip_names_keep_their_lines_apart` and
  `a_strip_name_is_one_bidi_paragraph` (chrome/strip/items_tests.rs),
  `a_paragraph_separator_starts_a_line_and_the_shaper_survives_it` (preview/markdown/tests.rs) and
  `a_control_character_in_a_name_leaves_the_chart_drawable` (drawer/monitor.rs).
- Text enters a cosmic-text buffer only through `set_buffer_text` and `set_buffer_rich_text` (render/text.rs), which
  make every bidi paragraph separator but `\n` and `\r` (U+001C, U+001D, U+001E, U+0085, U+2029) a space: cosmic-text
  ends a buffer line only at `\r` and `\n` and panics on a line holding two paragraphs of different directions.
  `rust/clippy.toml`'s text-into-the-shaper group refuses every other way in. Tests
  `the_shaper_gets_one_bidi_paragraph_per_line` (render/text.rs), `a_plain_preview_is_one_bidi_paragraph_per_line` and
  `a_table_cell_is_one_bidi_paragraph_per_line` (preview/markdown/tests.rs). An SVG's text reaches usvg under the same
  rule, each separator written as itself or as a character reference (`one_paragraph_svg`, preview/image/svg.rs), since
  usvg draws only a text chunk's first paragraph: `svg_tree` is usvg's one way in (an `.svgz` unpacked first), and the
  same clippy group refuses usvg's `Tree::from_data`, `from_str` and `from_xmltree` elsewhere; tests
  `svg_text_after_a_paragraph_separator_is_drawn`, `svgz_text_after_a_paragraph_separator_is_drawn`.
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
`agents/sot-fe/sot-fe-request.sh`, `sot_ui`, `agent.message`, `agents/sot-fe/sot-nav.sh`, `sot_oneshot_request`.
Uses: `DaemonLaneEndpoint`, `fe.lease`, `fe.leaving`, `fe.notice_seen`, `rust/frontend/src/lease.rs`,
`workspace.create`, `workspace.destroy`, `workspace.list`, `workspace.reauth`, `pty.input`, `pty.screen`,
`workspace.changed`, `FeAttachClient`, `rust/backend/src/rows/run/headless.rs`,
`rust/frontend/src/ui/agent_pane/attach.rs`, `sot_state_dir`, `sot_config_dir`, `host_name`,
`work_state::poll`, `tree.root`, `tree.children`, `directory.list`, `nav.toggle_hidden`, `preview.get`,
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
- `chrome/`: pane geometry and focus delegation, the status line, nav spill, colours, the wireframe and the frame's chrome draw.
- `chrome/strip/`: the session strip band under the panes.
- `input/`: FocusPrompts, the action catalog, key and mouse routing, help and clipboard paste.
- `nav/`: the navigation pane's trees (mode, store, Modules, Sessions, Hosts).
- `nav/files/`: Files mode's prompts, reveal walk, transfers and re-lists.
- `session/`: `RowDirectory` owns listings/identity, `RowPresentation` owns metadata/order/flashes and `ResultLedger`
  owns owed results/attempts; State coordinates effects, snapshots, switch, picker, presence and the badge floor.
- `agent_pane/`: `AgentPane` and its attach, input, screen and reply operations; thin State adapters apply effects.
- `drawer/`: the bottom drawer and its tenants.
- `drawer/repl/`: ReplModel owns row behavior; ReplPresentation owns image refresh, projection, layout, anchoring
  and painting; thin State adapters apply transport and presentation effects.
- `drawer/terminal/`: the Terminal tenant (local pty, vt100 helpers).
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
- `events.rs`: drain_events routes each IncomingEvt variant to its owner (a `Connected`'s REPL runs still running to the REPL drawer's `on_repl_running`, the rest of it to the session's `on_connected`), forwarding result-tree attempt identities unchanged to the navigation reply handler; event-service scheduling remains owned by the app.
- `connections.rs`: the window's view of its connection set: `send`, `send_to`, `default_host`, `ordered_hosts`.
- `page_proxy.rs`: arming a local listener so a remote daemon's page opens, at a port of the window's own for a page a
  daemon op served, and the URL that opens it (`ensure_proxy_for_url`, `PageSource`, `bind_proxy_listener`).
- `agent_pane/`: the agent pane (its own page).
- `app/`: the winit application and exit path (its own page).
- `chrome/`: the pane chrome (its own page).
- `control/`: agent control of the window (its own page).
- `drawer/`: the bottom drawer (its own page).
- `init/`: building the window's state at launch (its own page).
- `input/`: FocusPrompts, its behavior tests, key bindings, help and paste (its own page).
- `nav/`: the navigation pane's trees (its own page).
- `persist/`: settings, discovery and resume state (its own page).
- `preview/`: the preview pane (its own page).
- `render/`: the window's pixels (its own page).
- `session/`: which session row the window is on (its own page).

## Start here
`State` in mod.rs for view state; `app/handler.rs` for the event loop; `app/frame.rs` for one frame; `events.rs` for a
wire reply (then the `on_<variant>` it names); `input/keypress.rs` for a key. Each folder's page says its own rules.
