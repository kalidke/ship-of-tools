# rust/frontend/src/ui: fe-ui, the window (charter)

## Idea
One UI thread owns all view state and every other thread reaches it only by waking the window; ratatui computes cells
and one wgpu pass draws every pixel. The window's `State` (mod.rs) is driven by one winit loop. `session/row_directory.rs`
owns authoritative rows and identity; `session/row_presentation.rs` owns their presentation;
`session/result_ledger.rs` owns owed results and their attempts; `agent_pane::AgentPane` owns the selected agent pane.
`drawer/repl::ReplModel` owns REPL row state, submissions, history, routing and lifecycle;
`drawer/repl/presentation::ReplPresentation` owns window-wide REPL images, layout and scroll anchoring.
`input::FocusPrompts` owns window-wide focus, nav prompts, captured scale focus, stayed notice and quit hint.
`input::LayoutHelp` owns drawer selection, maximize/wide flags, Help browsing, return origin and startup requests.
`preview::requests::PreviewRequests` owns both request clocks, the fired preview target and its page transport, concept
follow commitment, the driven-preview hold, and the crop, scale-save and `--roi` aim transactions.
`preview::source::InstalledPreviewSource` owns the installed preview bytes, their node, scale and markdown origin.
`session::ParkedViews` owns the rows' parked views (`WorkspaceUiSnapshot`), keyed by row.
`session::SelectedRow` owns the selected `(host, workspace)`, its read-mark dwell, the destroy confirm and a create's
request time; `session::WorkspacePicker` owns the new-session picker's listing, cursor and account choice.
`nav::NavCursor` owns the nav cursor's settle debounce, startup/relaunch/reconnect placements, README default, pin and
project-scan clock; `nav::TreeView` owns a tree's rows, cursor and content generation; `nav::Navigation` owns the
active tree, its mode, scroll and scan root, and the store of parked trees; `nav::RevealWalk` owns the reveal walk's
target, awaited level, refresh memo and root-armed target; `nav::FileOps` owns the pending create and delete and the
upload batch. `preview::concept::DriftCheck` owns the drift badge's per-file parsed hashes, parse latch and backoff;
`preview::editor::state::Editor` owns the active edit, the file edit awaiting its `file.read` and the shaped edit buffer;
`preview::concept::ConceptSlot` owns the last annotation read, its shaped body and the concept pane's rect;
`flowed_selection::FlowedSelection` owns mouse selection over flowed text; `preview::shaped::ShapedPreview` owns the flowed preview buffer, its rect, scroll and definition-line
anchor, the math SVG, the refused-hello overlay and the highlighter; `preview::image::raster::RasterView` owns the shown raster, its
zoom, pan and dimensions, the visible ROI and the same-directory view carry; `preview::image::pages::PageView` owns
the shown preview's page, its raster zoom and a re-raster's keep-view; `preview::image::figure_overlays::FigureOverlays`
owns the scalebar toggle and label, the caption store and label, and the caption band the last frame reserved;
`preview::image::ink::InkLayer` (D4a, D4b, D4c1 and D4c2) privately owns the row's drawing, a `Drawing`
(base, committed strokes, the `cleared` copy, the undo history, drawing mode), the live stroke, drawing mode, a `revision` every committed change raises, the raster (`raster_dims` of the base's source size, which a press hands it, so a new base or size repaints it; painted with the wire's `paint_segment` and `mask`: a drag paints only its new segment, an undo, clear, erase, restore or new base paints all of it, and a hidden base's live stroke is painted out), the rectangle changed since the last upload and the overlay `Quad` (RGBA in the ink color, alpha from the raster; `upload` writes the dirty rectangle, or a new texture when the size changed); it
takes the canvas and image rectangles as arguments, keeps the last frame's for the pointer and never takes `State` (the window holds one, `State::ink`); a stroke begins only at a press inside the image and
canvas rectangles, and its drag points are clamped to the base and kept one raster pixel from the last; the drawing holds at most `MAX_INK_POINTS` points (a press
on a full drawing starts no stroke); a right-button erase drag is one undo step, recorded at its first hit, whose path from the drag's last point removes
each stroke within the painted ink's reach (`ink_radius` and one raster pixel), at points inside the image only (a point it ignores breaks the path, so the drag never joins the points either side of it, and the caller hands it every right motion of the drag); undo and clear act only while the drawing's base is the one shown; no event splits the strokes from the history;
the `cleared` copy goes at a stroke's commit and at a base change; a hidden base drops the live stroke; `sync` (run before each frame and pointer event) drops it when the shown base is not the drawing's, and repaints the raster, live stroke included, when the same base's source size changed
(`a_point_lands_on_the_base_where_it_was_drawn_at_any_zoom_and_pan`,
`a_stroke_ends_at_its_release_a_new_press_or_a_lost_focus_and_is_dropped_with_its_base`,
`undo_clear_and_restore_keep_their_order_and_a_new_stroke_drops_the_cleared_copy`,
`the_cleared_copy_goes_with_a_commit_or_a_base_change_not_a_press`,
`the_right_button_erases_in_one_undo_step_and_never_a_live_stroke`,
`no_event_between_an_erase_hit_and_the_drag_end_splits_the_strokes_from_the_history`,
`the_eraser_reaches_the_painted_ink_along_the_path_and_only_inside_the_image`, `a_drag_that_leaves_the_image_and_comes_back_joins_nothing_across_the_stretch_outside`,
`undo_and_clear_change_nothing_while_the_drawings_base_is_not_shown`, `the_drawing_is_bounded_in_points`, `the_overlay_is_the_drawings_raster_after_every_edit`);
D5, the canvas: the `Drawing` also holds whether the row's canvas is shown (`preview.canvas`, `Shift+d`, in the `Preview` scope, live while the preview is focused and not editing; it rides the parked view with the rest of the drawing), and `InkLayer::shown_base` returns `InkBase::Canvas` while it is shown, outside edit mode and the refused-hello overlay, so the press, keys, raster and overlay follow from the one decider; the canvas is a base like a figure (one drawing per row: a stroke on it replaces a figure's drawing, with the status line), its source size is `CANVAS_DIMS`, and drawing is always on over it (`Drawing` scope live; `Context::allows` refuses `Pages`, `Image`, `PreviewFile` and the text scroll keys while it is shown, so no key reaches the hidden preview, and the wheel, told by `preview_shows` through `WheelGeometry`, scrolls nothing under it); `shows` takes the canvas already decided by `shown_base`, the one decider of the precedence, and Shift+d does nothing under the refused-hello overlay; `preview::layout::shows`, behind `State::preview_shows`, gives the canvas the pane over the image and the text and under the refused-hello overlay and edit mode, so no text is prepared for it, while the installed preview stays installed and untouched under it; a preview of another node installed (`InkLayer::install_preview`, the call `install_preview_reply` makes in place of the source's own `install`, compares the installed node with the new one) hides the canvas, the same node's re-install does not; the pass paints a white `Solid::Canvas` rectangle letterboxed to `CANVAS_DIMS` in the image area and the overlay over it at the same rectangle (`the_canvas_takes_the_pane_and_leaves_the_preview_untouched` (the pane's decision, fed by `shown_base`, and the install call), `the_canvas_has_its_own_source_size` (`source_dims`), `the_wheel_scrolls_only_what_the_preview_pane_shows`, `the_canvas_is_a_base_like_a_figure_and_replaces_a_figures_drawing`, `the_canvas_flag_rides_the_parked_drawing`, `the_canvas_key_is_the_previews_and_the_canvas_takes_the_images_and_the_texts_keys`); the share of the drawing (D6b): `SelectionShare`'s `Want` is a `Share`, the released text or `Ink { base, revision }`, and `wanted_ink`, tried before the text in `desired_selection`, wants the row's drawing while `shown_base` equals the drawing's base and it has a committed stroke, identified by `InkLayer::revision` (`step` holds every request while a stroke or an erase drag is live, so nothing is sent during a drag); a drawing and a text selection both wanted send the drawing; the strokes are copied into the request only when it is sent; the request is `selection: None` with `drawing`, a clear is the same request with neither, and every ordering of the text share holds for it (a drawing replaces a text in place in the same row); the reply's status lines are `drawing shared: the agent sees it at its next prompt`, `drawing held: ...`, `drawing not shared: ...` by the code (`too_large`, `not_an_image`, `no_agent`, else the code or error) and `drawing not shared: this daemon cannot take a drawing; update it` for fe-net's unsupported reply; a clear earns none (`the_window_shares_its_drawing_and_withdraws_it`; the native fixture of a real window and agent is unbuilt);
the preview pass draws the overlay quad right after the raster's, at the same canvas rectangle and under the same scissor, only while `InkLayer::shown_base`, the one decider of the base on show, returns the drawing's base: the figure the shown raster came from, an image node, with edit mode off and the refused-hello overlay off (`the_shown_base_is_the_raster_node_only_for_an_image_outside_edit_and_the_refused_overlay`); a failed upload is logged once and skips the overlay until an upload succeeds, and `Quad::write_region` refuses an empty rectangle, one that leaves the texture, or too few bytes (`a_region_write_refuses_what_leaves_the_texture`);
`preview::image::ink_input` wires it (D4c1): a left press inside the image rectangle with drawing on over the shown base (`State::shown_ink_base`, which calls `InkLayer::shown_base`; no second decider) focuses the preview first, begins a stroke and starts no text selection but still clears the agent pane's selection (`SelectionContext::draw_press`, first in `press_selections`; `a_press_the_drawing_takes_focuses_the_preview_and_starts_no_selection`); the layer owns that routing (`left_press`, `moved`, `right`, `focus_lost`; `the_window_routes_a_left_press_motion_and_the_right_button_through_the_layer`): motion extends a live stroke, else a right drag erases, release commits, the window losing focus commits a live stroke and ends a held erase; a right press goes through the quit question and the stayed notice as a left press does and a release of either button never does (`a_right_press_answers_a_question_or_a_notice_and_a_right_release_always_reaches_the_eraser`); `sync` keeps a live stroke over a size change (`a_size_change_keeps_the_live_stroke_and_paints_it`) and drops it with its base (`a_base_no_longer_shown_drops_the_live_stroke_and_the_same_base_stays`), and a size change repaints the raster (`the_same_node_at_another_size_rebuilds_the_raster_at_once`); `preview.draw` (`d`, Image scope), `preview.ink_undo` (`u`, `Primary+z`) and `preview.ink_clear` (`Delete`) in the `Drawing` scope, live in the focused, not editing preview while drawing is on over a shown base (`Context::drawing`), resolve only there (R-D15, `the_drawing_keys_resolve_only_where_drawing_is`); the status line says `drawing on: ...`, `drawing off`, `drawing cleared: u brings it back`, `drawing: no image to draw on`, and the layer's own `the drawing on <name> was replaced` and `the drawing is full: undo or clear`;
limits: the draw order, the scissor and the texture upload are in the render pass and no pure test reaches them, and neither does the State routing: the node `RasterView` records at install (cleared with the quad at hide) and the argument the gate reads are unpinned, so a slow or failed reply or a failed decode keeping one figure's ink off another's rests on reading, and `write_region`'s transcription of origin, extent and row length into `write_texture`, and the overlay's resize and its dirty rectangle's clearing in `upload`, are unpinned; the window repaints the whole raster on the UI thread (the wire's `mask`, 36 to 243 ms at the point bound for a hand-drawn drawing on a 2000 x 2000 raster, 620 ms for 300 px steps, measured at the earlier 1/200 ink width; the 1/300 width visits about a quarter fewer pixels, so these are upper bounds), once per undo, clear, erase hit or new base and each return to a row whose drawing's base is shown; the canvas's white rectangle and its overlay (`paint_canvas`) are in the render pass and unpinned like the figure's; unpinned in D4c1: the State handlers (`press_ink`, `ink_moved`, `ink_released`, `ink_right`, the focus loss, the key handler) the render pass's `ink_sync` and `set_frame`, and the press handler's setting of focus, which rest on reading, as does `sync`'s call from each handler; D4c2: the row's drawing rides its parked view (`WorkspaceUiSnapshot::ink`, the layer's own `Drawing`; `park` and `restore`): leaving a row parks it, entering restores it (a first visit restores the default, so the layer starts clean), a destroyed row's view drops it with the rest; a restore keeps nothing else of the row left: no live stroke, held button, frame, raster or overlay (`sync` repaints the raster at the next frame), and raises `revision` (`a_parked_drawing_keeps_the_cleared_copy_and_the_undo_history`, `a_switch_drops_the_live_stroke_the_held_button_and_the_raster_and_raises_the_revision`, `a_restored_drawing_keeps_its_own_mode`, `a_rows_drawing_comes_back_with_its_view`); the State's three calls (the park, the first-visit restore, the return's restore) and the destroy's order are pinned by the native fixture `a_rows_drawing_stays_with_its_row`, which runs only under a display; a window's close, and a restart, lose every row's drawing (nothing persists it); the canvas's State wiring rests on reading: `preview_shows` taking `shown_base` as its canvas, `ink_src` handing `source_dims` the served size, the drawer's `canvas` and `drawing` fields, the Shift+d handler's refusal under the overlay, and the status lines `canvas: draw with the pointer; u undo, Delete clear, Shift+D back` and `canvas hidden`; not built: a source past 6000 px and over 20 MB (`PREVIEW_DOWNSAMPLE_TRIGGER`) is served downsampled and the preview reply carries no source size (`files/preview/mod.rs` folds the factor into `physical_scale` alone), so the raster is `raster_dims` of the served size and differs from the daemon's for such a source (the D4b review's N3)
`preview::image::figure_store::FigureStore` owns the markdown figures' bitmaps, requests in flight and failures;
`preview::markdown::media_store::MarkdownMedia` owns the table buffers and their scroll, the math and token caches
with their requests in flight, and the pending reflow; `control::ControlChannel` owns the fe-commands queue and the
last `fe-state.json` signature; `control::ReplyBar` owns the one reply a page offered, held for its person's word, whether
it is in view and its wait before Enter; `drawer::terminal::drawer::TerminalDrawer` owns the Terminal drawer's shell, its last
pty size and its starting folder; `drawer::monitor::MonitorPane` owns the Monitor drawer; `chrome::StripMotion` owns the session strip's eased scroll and the brand wheels'
spin; `chrome::NavSpill` owns the nav spill's deadline, collected segments and trigger cursor; `chrome::StatusLine`
owns the status text and the hold a pushed notify keeps on it; `chrome::BatteryGauge` owns the cached battery label;
`input::pointer::Pointer` owns the pointer's last position and the wheel's sub-row residue; `proxy_arms::ProxyArms`
owns the page proxy's proxied hosts, listener manager and armed listeners; `host_view::ActiveLink` owns the active
connection's hello facts and `host_view::HelloRefusals` each host's refused hello; `render::gpu::Gpu` owns the
surface, device, queue and surface configuration; `render::canvas::Canvas` owns the glyph text layer, the chrome's
cell grid and the cell metrics; `render::brushes::Brushes` owns the quad pipeline, the solid brushes, the cell
backgrounds' palette, the logos and the help peek's backing; `app::startup::StartupScript` owns the harness's command-line one-shots;
`render::capture_run::CaptureRun` owns the capture harness's run; `app::pacing::FramePacer` owns the frame-rate cap
and the first frame's focus.
Each of these owners lives in a module (or a module tree) that holds no `impl State`, so the compiler, not
convention, keeps the window's code out of its fields.
The remaining concepts below still reach shared fields through `impl State` blocks and free functions using `use super::*`.

## Owns
- The winit loop, frame pacing and the exit path: `app/`.
- The render stack (glyph text, the cell backend, textured quads, the surface and capture) and the frame's one render
  pass: `render/`, `render/pass/`.
- Chrome, key bindings, help and paste: `chrome/` (panes, status line, nav spill, strip), `input/`.
- The navigation trees: `nav/`. The session view and its switch: `session/`. The agent pane: `agent_pane/`.
- The REPL, Terminal and Monitor drawers: `drawer/`. The project chat's owner: `drawer/chat/`. The preview pane, images, markdown and the editor: `preview/`.
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
  workspace switches.
- `RowPresentation` privately owns labels, canonical-root fallbacks, state/stamp pairs, transition history,
  flashes, strip order and the active host's default. Projections use only `ordered_hosts` and listed host/slug keys;
  inert defaults retain metadata. None selections use hello metadata; unknown nondefaults get no root fallback.
  Stable host blocks, in `ordered_hosts` order, are sorted by a `StripOrder`, default `State`: rank blocked, badge,
  done, working, waiting, rest, then newest stamp, pinning the selection's previous slot. `Alphabetical` sorts a block
  by shown label without case, then slug, with no pin. `Recent` sorts by newest `agent_status_at` (an unreadable stamp
  last) and pins like `State`. `set_order` re-sorts at once without the pin; the key action `strip.cycle_order`
  (`Primary+Shift+O`) cycles state, alphabetical, recent and the choice is kept as `strip_order` in the resume state,
  applied at startup before the first listing.
  Known changes flash; missing rows retain history. Sessions/strip share the 600 ms fade. Destroy forgets only
  labels/roots; capture supports flashes without demo rows.
  The clock is sampled separately at each changed-row insertion after projection and at each explicit demo flash
  after capture seeding, preserving the timing of `1bc65e054`.
  Behavior tests: `workspace_list::tests` and `row_presentation::tests` (among them
  `alphabetical_order_sorts_each_host_block_by_label_and_ignores_state`,
  `recent_order_puts_the_newest_status_first_and_keeps_the_selection_in_its_slot`,
  `changing_the_order_resorts_at_once_and_does_not_pin`, `the_order_cycles_state_alphabetical_recent`,
  `next_row_follows_the_chosen_order`), and the resume round trip in `persist::resume_tests`.
- `ResultLedger` privately owns canonical owed results, the listed badge index, both checked serials and the
  hostless issuing reveal token. Replacements reset attempts; retries advance independently, and neither beginning
  an attempt, releasing reveal ownership nor abandoning an unsent attempt consumes the owed result. Admission
  requires the event dial host, active host, current listing, reveal ownership and both current serials. Cursor
  landing releases the marker and certifies the node; preview installation certifies only the matching generation
  and node after State's reply admission and source installation. Only the post-present operation acknowledges,
  with both certificates and matching displayed cursor/source nodes on the resolved row. Kept outcomes do nothing;
  removed outcomes invalidate the exact canonical identity. Prepared retirements reconcile only pending canonical
  identities before directory commitment; State applies returned reveal cleanup and badge refresh effects.
  The owner never takes State or exposes mutable storage. State retains navigation, sends,
  presentation ordering and persistence. Behavior tests: `result_ledger::tests`; the retained badge status test is
  `badge::tests::pending_nav_status_names_workspace_and_path`. The ten-fixture native registry also runs
  `replaced_result_rejects_old_tree_attempt_before_effects`, alongside the badge, retirement/disconnect and
  kept-default controls. Registration alone is not hosted execution evidence.
- `PreviewRequests` privately owns preview/concept generations, the concept fired target and driven hold,
  including private prepared changes and consumed commit tokens. It never takes State or exposes mutable storage.
  Preview and concept slots advance independently with the existing arithmetic; concept reloads share the concept
  clock without retargeting follow. ResultLedger calls a narrow lazy preview allocator only after serial admission
  and owed-entry lookup; absent results and refused serials consume no generation or badge acknowledgment.
- Reply continuations admit exact generation, dial host and optional workspace before source/page/save/status or
  ledger effects. Generation zero and repeatable current replies keep their existing admission. Concept replies
  update a matching stale editor before independently updating the matching cursor concept. A changed nonempty
  follow allocates before send, commits the target and clears caches only on success, then considers lazy drift.
  Failure consumes its number, retains target/caches and skips drift; targetless transitions clear without sending
  or advancing the clock; unchanged targets still consider drift. Matching annotation invalidation and view resets
  rearm follow without resetting clocks. Moved diagnostics retain their original targets, fields and ordering.
- Driven holds store the exact old cursor-row spelling and suppress only that derived preview candidate, at the
  existing follow point after pin/capture admission and target derivation. A different candidate releases the hold;
  landing, abort and departure clear only the hold. The owner never moves the cursor. Clocks and hold stay outside
  snapshots; restore, first visit and active reconnect reset only concept firing at their existing transition points.
  Portable proofs: `preview::requests::tests` and
  `session::result_ledger::tests::result_attempt_allocates_exactly_once_only_after_admission`; retained admission
  characterizations are `preview::fetch::tests`. These describe existing behavior, not a newly corrected defect.
- It also privately owns the fired preview target (the node the latest request asked for, distinct from the installed
  source's node), the target page, the one in-flight page request (turn or zoom re-raster), the crop host pin, the
  scale-save marker and the one `--roi` aim. Every page press, repeats included, advances the target within the
  document's bounds; it never advances from an intermediate shown page. While a page request is in flight, presses
  only change the target. Its admitted reply may draw an intermediate page, then only the latest target is requested
  if it differs. A reply clamps the target to a changed page count but never moves it back to the shown page.
  A new preview generation for a file or a scale save, forgetting/restoring the fired target, and leaving the row
  clear page intent and flight state. A turn cancels a re-raster's keep-view but waits for its reply; other re-rasters
  wait for the occupied page slot. An admitted failure releases the slot and tries only a target newer than the failed
  one, preventing error retries from looping; a failed send also releases it. Portable behavior test in the existing
  frontend unit suite: `preview::requests::tests::page_presses_and_repeats_coalesce_until_the_latest_target_is_shown`.
  Crop still takes its host pin before comparing, so a wrong host consumes it and two
  same-host crops are indistinguishable. Save success still requires current preview admission and target equality;
  ScaleSetFailed remains unfenced and clears its marker unconditionally. An aim is certified by an installed raster
  for its row and node, retired by a non-raster install, and consumed by the render pass only while it is still the
  fired target. No stronger transaction identity is claimed. The native result fixture reaches the fired target and
  the aim through the owner's operations; its ROI assertions stay intact.
- `InstalledPreviewSource` privately owns what the last admitted preview reply installed: its mime and bytes, the node
  it answered (what `o`/`W`/`O` act on, never the fired target), its physical scale and, for a markdown reply with a
  node, the document's node and workspace. It is one Clone value: a row's snapshot carries it whole and restores it
  before the repaint, and a first visit clears it whole, so the bytes, node and calibration are always the latest
  reply's. The markdown origin is not: it is the last markdown reply with a node, so after markdown A with a node, a
  PNG or a node-less markdown B replaces the bytes, node and scale and keeps A's origin (figure fetches for the shown
  markdown still need it). The coordinator repaints from an owned copy of the
  bytes; `figure_failed` stays with the markdown media and travels beside it in the snapshot.
- `ParkedViews` privately owns the map of parked row views. Leaving a row parks its view (replacing an earlier one),
  coming back reads a clone, a kept default row's end forgets only the parked attachment, a destroyed row's view is
  dropped, and the default row's raw-slug key migrates to `<default>` with an existing `<default>` view winning. State
  composes and applies the view; the map is reached through nothing else.
- `SelectedRow` privately owns the selected host and workspace, the pending read mark, the two-press destroy arm and
  the time a `workspace.create` was sent. Readers see the host and workspace by reference; only a switch's retarget
  and the capture harness's startup seed select a row, both parts at once. Every switch replaces the read mark (armed
  `READ_DWELL` ahead only when a person switched); each redraw takes a due mark whose row is still selected, and drops
  one whose row was left, before State sends the read flag. Every key press takes the destroy arm before its action,
  so only a second destroy press on the same row destroys. The create's time is taken by the next attach that switches
  rows. The native fixtures select their rows through the same operation. `WorkspacePicker` privately owns its host,
  path, listing, cursor, hidden-folder flag, return reveal and accounts: State opens and closes it and sends what its
  operations return (drill in, ascend, toggle hidden), a listing lands only when it answers the picker's own host and
  path, accounts only from that host with the first selected, and Tab cycles in range, so the native account fixture
  reaches each choice through the reply and Tab; an out-of-range index is the portable test's. The presence clock
  (`PresenceClock`) stays its own private-field value on State.
- `NavCursor` privately owns where the nav cursor lands and when its follow fires. Each frame asks it whether the
  cursor has sat still for `NAV_FIRE_DEBOUNCE`; a move restarts the wait, and a settled cursor lets the concept and
  preview follow fire. It hands out the one-shot startup selection, relaunch cursor and reconnect restore once each,
  marks a row's first Files root once (the README default, the marker migrating with the default row's key), holds the
  C2 pin (pinned, taken by an unpin, restored with a row's view, cleared by a first visit) and issues and reports each
  row's `project.scan` generation, a stale scan reply being dropped at its own row's latest.
- `TreeView` privately owns a tree's flat rows, its cursor and its content generation. Readers see the rows and cursor
  by reference; the cursor moves only by `select`, `select_clamped`, the arrow moves and the merges that re-anchor it
  by node id; a row is marked expanded only at request time (`mark_selected_expanded`, `mark_expanded`), so
  `apply_children` keeps dropping a reply for a collapsed parent; the root label is patched only through
  `relabel_root`. Tests build trees through the same operations.
- `Navigation` privately owns the active tree, the mode, the nav scroll, the active Modules scan root and the
  `TreeStore` of every other (mode, scope)'s tree, whose slots are private too. The store never holds the active key:
  a mode or row change sets the mode, then swaps (the departing view parks as user state with its scroll and, for
  Modules, its scan root; the entering slot comes back or an empty view does; a Files root shows the active row's
  label). A reply for a key not on screen parks: a root or a Sessions rebuild only into an empty slot or over an
  earlier parked reply (the decision is asked first, so the park log still precedes the rebuild's), a children splice
  or a scan into its slot. Parked trees are read by reference; a destroyed row's slots are purged and the default
  row's raw-slug slots migrate with an existing `<default>` slot winning.
- `RevealWalk` privately owns what the reveal walk remembers between round trips: its target, the ancestor it awaits,
  the one force-refresh it spent and a target armed for a row's incoming root. Each transition is named and ends
  exactly the parts it ended before: `start` (fresh levels), `reset_levels`, `stop` (target and level, the refresh
  memo kept), `abort` (with the memo), `clear` (with the root target: a row change, a failed result tree),
  `level_answered`, `await_level`, `mark_refetched`, `arm_root`, `defer_to_root` (reporting a root already in flight),
  `cancel_root` and `take_root`. `drive_reveal_step` and the tree replies stay State's, reading it by reference.
- `FileOps` privately owns the Files tree's in-flight file transactions: the pending Ctrl+N create and Ctrl+D delete,
  each settled only by a reply naming its node, and the upload batch with its one file in flight. The batch is pinned
  to the host and row it started on; the owner pops its queue, reads each chunk (`UPLOAD_CHUNK`) and returns it as the
  request's fields, counts a file done on its final ack, adopts the backend's resolved name from the first ack, and
  reports the host a failure must come from (the file's, else the batch's between files). A failed send or read, a
  backend failure and a reconnect end the upload and its batch; a local open failure ends only the batch. State sends,
  refreshes the destination listing and writes the status.
- `DriftCheck` privately owns the active row's drift cache: each `.jl` file's `file.parse` hash, the one-shot parse
  latch and failed parses' backoff records. A view takes the latch (re-armed after a failure's backoff, never past
  `FILE_PARSE_MAX_RETRIES`); a reply installs only into a latched, unforgotten path; a failure counts an attempt and
  keeps the latch; a failed send drops the latch; a `preview.changed` for the file forgets it; a row change and a
  reconnect clear it. The concept line reads one `DriftStatus` per file.
- `Editor` privately owns the preview pane's editor: the active `EditState` (its fields private: target, expected hash,
  header, baseline, buffer, discard prompt, stale banner, file node and version), the file edit whose `file.read` it
  armed, and the shaped buffer, rebuilt from the edit and absent without one. A key in edit mode returns one
  `EditorKey`; the discard prompt and the stale banner take only their answers, and a stale file reload arms its
  `file.read` before it is sent. A `file.read` opens an edit only for the armed node. A landed save snaps the baseline;
  a stale refusal raises the banner. A row parks the edit, never the shaped buffer or the armed read; a first visit
  closes it. State sends, re-shapes (`rebuild_edit_preview`) and repaints.
- `ConceptSlot` privately owns the concept slot: the last `concept.read` (`ConceptInfo`, its fields private), its
  shaped body and the rect it is shaped for. A reply installs the read and its shape together; a follow's commit and a
  first visit clear both; a row parks the read alone, and its restore drops the shape until the next read. The layout
  pass resizes the shape only when the rect moved by more than half a pixel. The shaped body is built and resized but
  no draw path reads it today.
- `flowed_selection::FlowedSelection` privately owns mouse selection over flowed text: the range as buffer line/byte
  positions and the drag. A pane lends its buffer, rect, text origin, clip bottom and an optional media placeholder
  character as a `FlowedText` per call and owns none of the selection; the owner's `hit`, `press`, `extend_drag`,
  `release`, `clear`, `text` and pure `rects` copy exact UTF-8 ranges (a soft wrap adds no newline, a placeholder adds no
  text) and clip the tint to the paint bounds. Tested by `flowed_selection_tests.rs`, where a fixture pane that is not
  the preview gets the preview's copied text and rectangles for the same wrapped Unicode text.
- `ShapedPreview` privately owns the pane's flowed text: one buffer shaped for the rect the last layout pass gave it
  (resized only when the rect moved by more than half a pixel), the math SVG beside it, the scroll in body lines, the
  definition-line anchor, the refused-hello overlay and a `FlowedSelection`. Its `hit`,
  `press`, `extend_drag`, `release`, `clear_selection`, `selected_text` and pure `selection_rects` operations lend the
  selection the text origin, top padding, scroll and media placeholder; the render pass tints the clipped rectangles before painting glyphs.
  Installing a buffer clears selection, drops the SVG and scrolls to the top. A row switch, Escape in the preview
  and opening edit mode also clear selection. `released_selection` answers the file's text: a plain or token buffer's
  own range and text, rendered markdown's source lines whole (`TextOrigin::Rendered`), and nothing for a rendered buffer
  with no map (`TextOrigin::Unshared`). The text scope's `preview.copy_selection` copies rendered text on
  Primary+C or Primary+Shift+C; `y` still copies fenced code sources. Selection does not cover MarkdownMedia's wide
  table buffers, math images, image previews, the concept pane or double-click word selection. A
  sent `preview.get` arms its anchor; the reply consumes it and scrolls to the definition only for a code reply to the
  request that armed it; a cursor move within the shown file re-anchors only on a real change of line. Scrolling is
  named: by a signed step stopping at the top, saturating line steps, top, end (clamped by the next layout).
- `RasterView` privately owns the image pane's view of one raster: its texture, loaded and served dimensions, zoom,
  pan, the visible source-pixel ROI, the per-directory, per-size view carry and the carry parked at install. A decoded
  raster installs; unless a page re-raster keeps the view, the view resets to fit and an image node's cached
  neighbor view is parked. Each frame clears the ROI and, with a raster shown, frames the canvas: a ready `--roi`
  aim wins over the parked carry, a degenerate solve is logged and dropped, then zoom and pan are clamped to the live
  geometry. State computes the ROI for an image node, which the owner records and writes through to the carry
  outside the quantization window; State echoes an applied aim and draws the quad. Keys reset, pan and step the
  zoom (1.25x below 1.5, whole steps above, pan scaled to keep the center).
- `PageView` privately owns pagination (ADR 0021): a reply's page extras set the page or clear it; a reply that
  re-rastered the page on screen records its zoom and arms a one-shot keep-view the raster install takes; any other
  resets the raster zoom to fit. A row's parked view keeps its page, and its restore applies it before the repaint
  (a first visit clears it), so a restored document keeps its sampler, its page keys and its title
  (`a_restored_row_keeps_its_pdf_page_and_a_row_without_pages_gets_none`). `FigureOverlays` privately owns what is drawn over a figure: the sticky scalebar
  toggle (disabled when a scale save fails), the caption store, the labels shaped each frame outside the render pass
  (cleared whenever their overlay is not drawn), and the caption band published before the image rect is derived, so
  the keyboard zoom ceiling uses the same reduced pane.
- `FigureStore` privately owns the markdown figures by literal URL: the decoded bitmaps (kept across navigation),
  the fetches in flight and the terminal failures. A fetch is sent only for a URL that is none of the three; a reply
  settles its URL, then installs or fails it; an unresolvable local URL fails at once. Failures report to the walk as
  zero-size metrics, are cleared only by a fresh markdown reply or a first visit, and a row parks and restores its
  own.
- `MarkdownMedia` privately owns the rest of the markdown media. Math and token requests go out only for keys
  neither cached nor in flight; a reply caches, settles its key and asks for one reflow (a math failure settles with
  no reflow, so it cannot loop); a figure's landing or failure asks for one too, and the frame takes it once. Table
  buffers are rebuilt only when their sources change, which also returns the shared horizontal scroll to the left;
  the scroll is clamped each frame to the widest table. A math SVG rasterizes once, at the pane's size; an inline
  math raster is sized to fit the preview pane's width and its reservation's height, never wider than the pane
  (`a_formula_wider_than_the_pane_rasterizes_at_the_pane_width`).
- `ControlChannel` privately owns the window half of the control channel (ADR 0019): the queue the command-file
  watcher fills through its producer end, drained whole under the lock alone so dispatch never holds it, and the
  signature of the last `fe-state.json` written, so the file is rewritten only when the state it reports changes.
- The `docs` command carries the opener's `direct` reply scope unchanged through parse, dispatch and the outgoing
  `docs.open` request on the sending daemon's connection. It defaults to `none`; row scopes carry their workspace,
  which the daemon resolves to a canonical id, and `all` is explicit. Ordinary UI opens pass `none`. An older
  frontend that discards the field grants no direct authority; the page reads `states.reply_scope` to learn what
  the daemon granted. Landing A carries scope only: every reply still reaches the confirmation bar below.
  Proofs: `control::command::tests::docs_scopes_reach_the_encoded_request_and_an_old_frontend_grants_none`, the
  transport's `every_request_kind_and_its_error_reply_match_the_golden`, and the native
  `result_target_uses_host_and_listed_identity` fixture's docs route (parse, dispatch, encoder and ordinary UI open).
- `ReplyBar` privately owns a reply a page offered (since 2026-10-27 a daemon of this tree offers none: an unscoped
  page's reply is refused, pages' contract; the bar serves only an older daemon and is a deletion candidate at this
  subsystem's next pass): the daemon it came from, the row's id and listed name, the handle
  and the exact line, how the bar is laid out (covered by a dialog, cut off, or whole at a panel rectangle) and when
  a presented frame last showed it whole. It never takes `State`. At most one offer is open: a second is dropped and the status line says so. `State`
  (`control/reply_dispatch.rs`) opens the bar only for a row `RowDirectory::resolve` finds listed on the offering daemon
  by its exact id (else the offer is dropped and said), requests the user's attention, tells the bar how it is laid out
  before each key and with each frame's overlays, tells it when a frame has been presented (`redraw`, right after
  `frame.present()`), and applies what it returns. A line that is not exactly
  `page_reply::check_line`'s opens no bar, so the panel shows what will be typed. The bar is the only sender of
  `page.reply`: Enter returns a `ReplySend`, a value only the bar can make, and `OutgoingReq::PageReply` is made from
  nothing else; `FeCommand::PageReply` is `skip_deserializing`, so the command file cannot open a bar, and
  `route_fe_command` makes it only from a directed `fe.command`, so a broadcast opens none. Enter sends only once a presented frame has shown the bar whole for one second and is not a repeat: layout is no
  evidence, so a frame that fails before it presents leaves the bar unshown and Enter sends nothing, and a slow frame
  does not shorten the second. The evidence is dropped whenever the layout changes (a dialog over the bar, a cut) and on every size event
  or text-scale change itself (`State::resize`, `apply_text_scale`), even when the layout comes back to the same
  rectangle before anything looks, and the second starts again from the next presented frame. Esc drops it; while the bar is not covered it
  takes every key, plain Enter and Esc by their logical keys and never a binding. Keys follow what is drawn on top:
  while the close dialog (the quit question, a leave's line, a stayed relaunch's notice or the quit hint) is drawn it
  takes every key and the bar none, and nothing under a covered bar gets one (a stayed notice is dismissed, the hint
  and a leave's line swallow the key; `covered_key_continuation`), so no Enter given to a dialog or typed while the
  bar was hidden sends a reply or reaches a pane. A window too short, too narrow (under 20 text cells) or otherwise
  unable to draw every row of the target and the line whole can drop the bar but not send it. The panel is the close dialog's renderer (`CloseDialogText::Reply`), titled "Reply from a page", under the
  close dialog; the target (handle, listed name, host) and the line go through `cell_lines` and `wrap_status`, so a
  hostile name stays on rows of its own with no control character in a cell. The mouse takes no part. The answer comes
  back as `IncomingEvt::PageReplied` and `page_reply_status` puts it on the status line, held like a pushed notify;
  the selected host's `Disconnected` lines that follow before a paint (a redial failing again) carry the held text,
  kept as a part of its own, instead of overwriting it (`session::replies::show_disconnect`, tests
  `the_disconnect_line_carries_the_text_held_on_it` and `a_reply_outcome_survives_any_number_of_disconnect_lines`): only `sent` says the line
  and its Enter were recorded. Behavior tests: `control::reply_bar::tests::{the_bar_sends_only_on_an_armed_enter,
  esc_drops_and_sends_nothing, the_bar_takes_every_key_while_open_and_in_view, a_second_offer_while_one_is_open_is_dropped,
  an_offer_whose_line_is_not_the_daemons_marked_text_opens_no_bar, the_bar_shows_the_target_and_the_exact_line,
  a_window_too_small_for_the_line_can_drop_the_bar_but_not_send, a_dialog_over_the_bar_takes_every_key_and_the_wait_starts_again,
  the_waits_deadline_is_only_while_the_bar_is_waiting,
  the_wait_starts_from_a_presented_frame_not_from_layout, a_resize_drops_the_evidence_that_the_bar_was_shown,
  a_size_event_drops_the_evidence_even_if_the_layout_comes_back}`, `control::command::tests::{a_directed_page_reply_opens_the_bar_and_a_broadcast_does_not,
  a_command_file_cannot_open_a_reply_bar}`, `input::keypress::tests::{the_quit_dialog_takes_a_key_before_the_reply_bar_and_the_bar_before_the_panes,
  nothing_reaches_a_pane_under_a_covered_bar}`
  and `control::replies::tests::the_status_line_says_what_became_of_a_confirmed_reply`; the native State fixture
  `the_bars_enter_sends_page_reply_to_the_pages_host` (a reply from host B's daemon opens the bar, every other command
  and a second offer send nothing, an Enter before any frame has presented the bar sends nothing, nor one straight
  after the first frame, a size event or a quit question closing over the bar, the teardown's `unknown` and the host's
  disconnect drained in one frame both show, and an Enter a second after a
  presented frame puts one `PageReply` with the line on B's connection and none on A's).
- `TerminalDrawer` privately owns the Terminal drawer's local shell (G2/G3): spawned the first time the drawer is
  shown, at 80x24 in the repo root; pumped before each draw (the report says when the pane looks wrong); its pty
  resized only when the drawer's cell size changes (at least 2x2). Typed and pasted input returns the view to the
  live tail; a wheel the shell's app tracks is forwarded as is.
- `MonitorPane` privately owns the Monitor drawer (ADR 0020): its chart model, the raster, the drawer rect, the
  readings' dirty mark, the subscription this window holds and the declared hub it asks. Each reply marks the
  readings dirty; the layout pass plans a chart when they changed or it was drawn for another size, text scale or set
  of marks, State rasterizes it and installs the result (a failed raster still records what it was drawn for, so it
  is not retried every frame), and another tenant drops it. The subscription's lifecycle (toggle, `--start-monitor`,
  resubscribe on a hub connection) is State's in `drawer/monitor_drive.rs`; the native fixture there sets up a fresh
  pane through its constructor and reads the raster through `quad()`.
- `StripMotion` privately owns the session strip's motion: the scroll snaps to the active session on the first
  paint and then eases toward it, and a workspace cycle flicks the wheels (clamped), which spin down; both step on
  one frame clock and report whether they still move, so the frame loop stays alive only until they settle.
- `NavSpill` privately owns the nav spill (`[nav] spill_ms`): each frame's cursor is observed, and only a user's
  move (same mode, workspace and tree generation, another cursor) arms the deadline; boot, loads, restores and
  switches move the baseline without arming. The draw publishes the overflowing rows it collected; the idle tick
  expires the deadline once, with a repaint.
- `StatusLine` privately owns the status line: every write replaces the text; a pushed notify (and the launch
  notice, a capture's result, an interrupted upload, the fullscreen pin, the session order) also holds it for `NOTIFY_STICKY`, during
  which `rebuild_connection_status` leaves it; the idle tick expires the hold once and rebuilds the line.
- `BatteryGauge` privately owns the battery readout: the OS is asked at most every 30 s, the label is `None` with
  no battery or on an error (never a fake reading), and every frame between queries reuses it.
- `Pointer` privately owns the pointer between events: the last position (winit reports it only on motion) and the
  wheel's fractional rows, stored by the wheel adapter and cleared when the agent pane consumes a gesture.
  Mouse routing focuses the agent or flowed-text preview before anchoring its selection; an outside left press
  clears each pane's selection, motion extends its active drag to the visible edge, and release ends both drags.
- `ProxyArms` privately owns the window half of the page proxy (ADR 0035): a host's pages are proxied exactly when
  its own `Connected` said so (no host's event touches another's); a page's listener is armed once per (host, daemon
  port), re-armed on reuse, and an `Announced` page whose port is held, by this window's listener for another host or
  by an unknown process, is never opened. State resolves the dial and reports each refusal on the status line.
- `ActiveLink` privately owns the active connection's hello as the status line and version stamp show it (label,
  project root and its basename, backend version, revision), set only by the active host's own `Connected` and
  relabelled by a switch to an already connected host. `HelloRefusals` privately owns each host's refused hello,
  lifted only by that host's own clean hello; the active host's is the blocking overlay.
- `Gpu` privately owns the window's GPU (the render resources' first stage): the surface, device, queue and the
  surface's configuration, dropped in their original order. The size changes only by `resize`; `acquire_frame`
  reconfigures a lost or outdated surface once and asks again. Device, queue and configuration are lent immutably to
  the drawing code.
- `Canvas` privately owns the render resources' text stage: the glyph text layer, the ratatui terminal over the cell
  backend, the startup scale, the text-scale multiplier (clamped to 0.5..=3) and the cell metrics derived from them
  (the cell width measured from the monospace advance). A text-scale change or a resize re-derives the chrome grid
  from the surface size in the same operation, so the cell count never disagrees with the cell size. Shaping
  (`shape`, which lends the font system to one buffer build or resize), the chrome draw (`draw_chrome`), text and
  overlay preparation and their paint, and the atlas trim are its operations; the cell backend is read through
  `chrome()`. The native pane-mapping fixture checks rejection and clamping against the window's own metrics, with
  points outside any geometry.
- `Brushes` privately owns the render resources' quad stage: the pipeline (lent immutably to whoever builds a quad of
  its own), one solid brush per colour drawn in a pass (a paint rewrites its brush's vertex buffer), the border
  brushes built once per colour before the pass that paints them, the strip logo and nav wordmark (each drawn only
  when it decoded), the help peek's backing, rebuilt only when its alpha changes, and the cell backgrounds' palette,
  one texel per colour, rebuilt before the pass only when the frame's colours change and painted first in the pass
  in one draw (at most 2048 colours a frame have their own texel; a run in a colour past that paints in the nearest
  colour the palette holds). Its bytes are pre-encoded for a surface that does not encode sRGB on write, so a
  background shows its own colour on either surface (the other solid brushes still show darker there). Painting is
  its operation over the `Gpu`; `Gpu` also holds the surface format's clear colour.
  Every texture is made through `Quad::from_rgba8_with_sampler`, which refuses one past the GPU's
  `max_texture_dimension_2d` or of zero size with an error naming its size, so no caller's size can panic the window
  (`a_texture_past_the_gpu_limit_is_refused_not_panicked`).
- A terminal cell (the agent pane, the Terminal drawer) is drawn as a terminal draws it: `paint_terminal` carries its
  background and inverse, and the cell backend (`render/cells.rs`, `cell_colors`) exchanges an inverse cell's
  resolved colours, the default foreground and the surface standing in for the side it leaves unset, so its letter
  shows on a bar of its foreground; the block cursor is an inverse cell and keeps the letter under it. Backgrounds are
  rectangles under every figure, selection and glyph (`project_backs`). The cells a wide glyph covers, which ratatui's
  diff never sends, are blanked in its style, so its background spans its width. A box-drawing cell's arms are not
  dimmed by DIM, as its glyph would be (docs/defects.md: TERMINAL-DIM-BOX-ARMS). Tests: `render::cells::tests`, `render::brushes::tests` and
  `drawer::terminal::vt::tests::{an_inverse_bar_keeps_its_letters,
  a_cell_background_reaches_the_backend_and_the_cursor_keeps_its_letter}`.
- `StartupScript` privately owns the harness's one-shots: `--auto-expand`, `--auto-pin` and `--demo-repl-eval` each
  fire once when the startup selection has landed on a non-empty tree (taken whether or not their action queues),
  `--demo-function-methods` once its row appears, and the `--start-path` walk's target and frontier memo, ended on
  arrival or a dead end. The walk itself, which reads the tree and expands, is State's.
- `CaptureRun` privately owns the capture harness's run: the `--capture` PNG, a pending selfie, the
  `--capture-preview` one-shot (and that it was given, which keeps the cursor's own preview standing down), the
  `--capture-cycle` steps (taken once) and the frame count. Each frame asks it for its shot: the harness shot at its
  target frame (`--capture-delay-ms`, else four times `CAPTURE_FRAME` with a capture preview, else `CAPTURE_FRAME`)
  beats a selfie on the same frame; the readback and the exit are State's. The native fixtures read the frame count
  through a reader compiled only for them.
- `FramePacer` privately owns the frame-rate cap: a redraw inside `FRAME_BUDGET` of the last frame is deferred
  (dirty) and drawn at the boundary, so a burst collapses into one frame (the capture harness draws every redraw);
  animations and debounces mark it dirty; the first drawn frame takes the window's focus once.
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
  lifecycle retains raw slugs. A destroyed row's parked record is forgotten at State's existing cleanup point, under
  `(host, slug)` as given (a destroyed row is never a default row, so no default-slug collapse), by the window's own
  destroy reply and by the sweep of every row a listing drops.
- A selected row that is destroyed is left when the window learns of it: by the window's own destroy reply, or by a
  `workspace.list` that no longer has a row the host listed before (another window, the command line, a row's end).
  Both ask `RowDirectory::leave(hosts, selected, event_host, Gone)`, decided before `commit_replace` consumes the
  listing, which names the next session in the whole Sessions list (the computers in `ordered_hosts` order, each
  listing in the daemon's order, default first then by slug), below the selected one first, else the nearest above it,
  among visible rows that are still listed and not noted as destroyed. A destroy is noted when requested
  (`State::request_destroy`) and when answered; a note ends when a listing lacks its row, or when the destroy is known
  to have left the row alive or its outcome is unknown: the daemon's refusal, a `kept` answer for that row, and the
  loss of the computer's link (`destroy_answered`, `clear_destroyed`, `note_connection`). `apply_leave` first makes the
  pane paint as a new one (`AgentPane::show_no_attachment`), so the gone row's last screen is not held while a cold
  neighbor attaches, then switches through `switch_to_workspace`; with no such row anywhere it goes to the event host's
  default row, and a switch to a row with no attachment shows the empty pane too. After a listing, every row it
  dropped is forgotten (`dropped`). A destroy of any other row, of another computer's row, or one the daemon refused
  leaves the view as it is. Limits: the dying row stays in view from the request to the reply or list that tells the
  window it is gone; a visible default row that is destroyed in view is kept by the daemon and stays selected with its
  pane ended (no move); a selection resumed before its host's first list is never "gone" (a row destroyed while the
  window was closed stays selected until the person moves); the neighbor may be a row of a computer that is down; and
  a neighbor or default row whose `resolve` refuses leaves the view where it was (the refusal is shown). Pins: the
  `leave`, `destroy_answered` and `clear_destroyed` tables and the pane tests in the unit suite; the calls into them
  (`request_destroy`, `note_connection`, the reply, the sweep, `apply_leave`'s clear and move and the key of the sweep) by
  `a_destroyed_session_moves_the_view_to_a_neighbor` and
  `a_destroy_in_flight_and_a_dropped_row_are_handled_by_the_window` in the native State fixtures (the
  `window_minimized` target: the per-landing hosted run executes them alone on Linux under a virtual display and
  Mesa's software Vulkan driver (`child fixtures`), and the hand-dispatched release gate runs the whole target on Windows
  and macOS; the local gate compiles these fixtures but does not run them).
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
  destruction still reconciles the ledger before the move and cache cleanup. The exhaustive router is unchanged.
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
- `LayoutHelp` privately owns drawer/maximize/wide flags, Help browsing, the first return origin and startup
  requests, including private nested storage. It returns owned effects, never takes State and lends only immutable
  observations. The drawer has four tenants (Repl, Terminal, Monitor, Chat) besides Help; Chat shows the held records (`drawer/chat/`) and
  its keys reach no REPL handler (`input/CLAUDE.md`). Ordinary toggles alone abandon the origin and request Monitor sync, after focus and before binding
  feedback/redraw. Automatic REPL opening keeps setting/base-preset gates; demo opening ignores them; Terminal
  failure closes only selection. These paths preserve other layout/Help state and keep held subscriptions.
- Maximize follows focus, yields to a leave line without clearing, and leaves spatial navigation on the preset.
  Context-admitted restore peels maximize before gated wide restoration. Wide enable bounces only Llm; admitted
  agent reveal clears wide before focus/paste without dismissing prompts. Help resets browsing, saves only the first
  focus/drawer/maximize origin (never wide), and consumes it once on close. Startup starts peek then drawer before
  draining events. Peek keys retain captured context/repeat rules; focused browsing passes window action scopes.
  Browser status precedes redraw; logs retain global_keys targets/fields/order. Overlay preparation stays after chrome
  draw, suppresses dialog spill and clears stale/expired peeks or promotes for insufficient space. Idle retains the
  five-second hold, 240 ms fade, 16 ms fading cadence and hint deadline. State retains settings, geometry, resources,
  persistence and external effects. Behavior tests: `input::layout_help_tests`, the Help characterizations and
  `app::handler::idle_tests::peek_deadlines_wake_fade_and_expire_without_postponement`. Native Monitor fixtures use
  public selection operations with independent subscription setup; fresh Windows/macOS release gates cover 7g/7h.
- `AgentPane` privately owns the host/attachment target, size, selection/drag, selected client, host-bounded warm
  pool, held keys by row, presented live run and dwell, hold, dial reason, request/client clocks, warning count, feed,
  discarded-input count and composed `PanePresentation`. Its operations never take State or expose mutable
  clients/screens/storage. The narrow
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
- Typed keys and pastes for a selected row are held locally until its installed Capsule client is checkpointed,
  attached, alive and its screen has been presented. Held input has a visible count and safe text tail, followed by
  the Enter/Esc hint. Once input is held, further typing stays behind it; only a fresh, plain Enter after a presented
  live screen and the one-second `HELD_SEND_DWELL` sends exactly the held bytes, as one input. Enter before the screen
  is live is held as CR; an early or repeated live Enter sends and adds nothing. Esc drops the held inputs and reports
  their count; Backspace removes the last key or whole paste. Keys are interpreted logically and without modifiers.
  Held keys belong to the host/attachment `WsKey`, survive switches, warm eviction and kept-row invalidation, and are
  removed by host-local row pruning. The total held bytes per row are bounded by capsule's `HELD_INPUT_CAP`; an input
  over that bound is refused whole and counted once, as is input with no selected target. The overlay makes every
  control, separator and bidi control safe for a cell and strips bracketed-paste markers from its displayed text.
  `live_since` starts only after present and is cleared on a nonlive observation, switch, installation or changed
  screen run. Every live key, paste and mouse sequence names the checkpoint's `screen_run` through
  `send_input_in_run`. Before sending held keys, a different worker `run()` keeps them and clears the dwell, reporting
  that the restarted row's screen must show again. Limit: a worker that moves after this check can still reject the
  send as counted `RunEnded`, with its existing notice; unknown delivery is never retried automatically. The worker's
  own pre-attach queue remains bounded independently; the pane no longer relies on it. A wheel while nonlive only
  scrolls locally or reports Pending and never holds mouse bytes. Behavior tests in `agent_pane::owner_tests`:
  `keys_typed_while_a_row_connects_are_held_and_sent_only_on_an_armed_enter`,
  `held_keys_stay_with_their_row_across_a_switch`, `held_keys_are_not_sent_into_a_run_the_screen_did_not_show`,
  `held_keys_past_the_bound_are_refused_and_counted`, `esc_drops_and_backspace_removes_the_last_held_key`,
  `a_repeated_enter_never_sends_held_keys`, `a_wheel_while_connecting_holds_nothing`,
  `held_keys_show_safe_text_and_the_tail_that_fits`, `a_redial_or_new_screen_run_requires_another_presented_frame`,
  `ordinary_input_and_wheel_name_the_shown_run`, `pruning_drops_only_the_removed_hosts_held_rows` and
  `kept_row_invalidation_preserves_the_existing_scope`.
  Held pixels never receive input. Copy uses client-or-blank and clears selection only after clipboard success.
  A copy the selected row's output asks for (OSC 52, Claude Code's `/copy`) is taken from the selected client
  alone, once that client has drained the output that waited for it at its install (`copy_quiet`: a warm client's
  backlog is an earlier view of the row), and a copy whose sequence began in that backlog is dropped too
  (`copy_tail`, while the parser is mid-sequence); State decodes it (`copied_text`) and writes this window's
  clipboard, the status line saying the count or why not. Limit: output the row sent while off screen and the window
  had not read yet when it came back counts as new. Proof:
  `agent_pane::owner_tests::a_copy_is_taken_only_after_the_installed_clients_backlog`.
- Every copy to and paste from the OS clipboard goes through the window's one handle (`input::write_clipboard_text`,
  `read_clipboard_text`), made at first use and kept for the process's life, since on Linux (X11) a copy is served
  by the handle that set it; a handle whose write failed is made again at the next use.
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
Each connection is one row of docs/integration.md, owned by its provider. Provides: `fe.command.send`, `fe.command`, `page_reply` (the reply bar),
`agents/sot-fe/sot-fe-request.sh`, `sot_ui`, `agent.message`, `agents/sot-fe/sot-nav.sh`, `sot_oneshot_request`.
Uses: `DaemonLaneEndpoint`, `fe.lease`, `fe.leaving`, `fe.notice_seen`, `rust/frontend/src/lease.rs`,
`workspace.create`, `workspace.destroy`, `workspace.list`, `workspace.reauth`, `pty.input`, `pty.screen`, `page.reply`,
`workspace.changed`, `FeAttachClient`, `rust/backend/src/rows/run/headless.rs`,
`rust/frontend/src/ui/agent_pane/attach.rs`, `sot_state_dir`, `sot_config_dir`, `host_name`,
`work_state::poll`, `tree.root`, `tree.children`, `directory.list`, `nav.toggle_hidden`, `preview.get`,
`preview.set_scale`, `image.crop`, `concept.read`, `concept.write`, `concept.list`, `file.read`, `file.write`,
`file.delete`, `file.download`, `file.upload`, `dir.create`, `preview.changed`, `repl.eval`, `repl.run_file`,
`repl.interrupt`, `repl.execute`, `kernel.request`, `math.render`, `pluto.open`, `monitor.subscribe`,
`monitor.unsubscribe`, `monitor.history`, `repl.frame`, `monitor.tick`, `video.open`, `docs.open`, `quarto.open`,
`proxy.connect`, `ensure_proxy_for_url`, `pipe_one`, `loopback_port_from_url`, `rust/protocol/src/page_url.rs`,
`OutgoingReq`, `IncomingEvt`, `HostTable`, `lane_dial`, `LinkHandle`, `ResolvedDial`, `--socket`, `--dial`, `--relaunched`,
`relaunch.request`, `spawn_watcher`, `rust/frontend/src/relaunch.rs`, sot_protocol::annotation::split_frontmatter, sot_protocol::annotation::synced_against, sot_protocol::physical_scale::PhysicalScale, sot_protocol::physical_scale::parse_physical_scale, sot_protocol::video_path::video_mime, lease_notice, handle_fe_command_send relay diagnostics.

## Chat owner
`ChatOwner` (`drawer/chat/`) holds one watched channel (its host, target, fill: `Needed`, `Reading`, `Complete` or `Gap`, and
its records), the one post in flight and the last post's outcome. It sends nothing itself and replays no mutation: each method
returns the requests for State to send. Rules, each with its test in `drawer/chat/owner_tests.rs`:
- One recovery serves every loss: the link returning, `bus.lost` for `chat.changed`, a `chat.status` that lists the channel or
  says the service is not running, and an `Unreadable` event. It forgets the registration and sends `chat.watch`
  (`a_lost_chat_bus_marks_the_state_incomplete_and_watches_again`, `a_status_is_answered_by_a_paced_watch`,
  `an_unreadable_chat_event_watches_again`). Only a watch reply that says `complete`, while the state needs one, starts the one
  `chat.history` read; a pending watch is sent again by `tick`. Watches go out at 0, 1 s, then every 5 s; a recovery keeps its start and pace through a loss other than the link (a link change starts over), and a `complete` snapshot ends it.
  A refused watch is a gap at once; for a watch the daemon admits or never answers, `Gap(reason)` is shown only after the daemon's slowest snapshot: 20 s from the first watch it admits (any answer but `unavailable`; with none, from the recovery's start, so a watch never answered still ends in a gap), the last watch getting its full pace to be answered
  (`a_slow_last_watch_still_ends_complete`), and a refusal of `unavailable` (the service may be starting) is asked on that pace for 20 s before it is
  `Gap(Some(Unavailable))`; a snapshot that never completes is `Gap(None)`, and a watch `complete` whose reads are all refused is `Gap(Some(<the read's code>))`; a `pending` reply clears the read's refusal, in a gap or before it. A recovery is reset (`Watch::begin`) by either change of the link and a loss from `Complete`, and a new watch starts with the fields `begin` sets (`a_gap_comes_only_after_the_daemons_slowest_snapshot_and_says_why`).
  A stream of losses whose recoveries succeed is paced, not capped. A watch refused `stale_row`, `stale_mapping` or `unmapped` is
  `Gap(Some(code))` and is not asked again until a row or link change; any other refusal is `Gap(Some(code))`
  (`a_refused_watch_is_a_gap_and_a_duplicate_reply_starts_no_read`). `Gap` is shown and asked again: a watch goes out every
  minute with no cap, because the daemon answers a watch from its state at that moment and says nothing later, and each
  watch also starts its retry of a failed snapshot; a `complete` reply reads as from `Needed`, a refused one stays `Gap`
  (`a_gap_is_asked_again_every_minute_and_a_complete_reply_ends_it`); `too_many_watches` and `unavailable` keep this ask on purpose, since each can clear. The state is not whole while the link is down.
- A `chat.changed` that came while the read was in flight is kept over its older snapshot (`a_change_during_the_read_is_kept_over_the_older_snapshot`);
  an event from before a refused read, a link loss or with no read in flight is not (`only_the_events_of_the_read_in_flight_cover_a_snapshot`).
  The read asks for the wire's page (`MAX_PAGE`).
- A snapshot replaces the held records, and its `next` cursor (older history, the drawer's to page) is not a gap
  (`a_snapshot_replaces_the_held_records_and_a_next_cursor_is_not_a_gap`). The snapshot keeps the channel's top-level messages, as the
  daemon's model does (it sends no edit or delete for a thread reply sent to the channel). The owner holds at most the daemon
  model's newest 100 records (`KEPT`), dropping the oldest as it does (`the_owner_keeps_at_most_the_daemons_newest_records`). A
  `floor` (set by a snapshot that is not the whole channel, raised by the trim) bounds what is added: a `put` of a record not held
  and older than it is not added, the daemon's own rule for an edit below its set (`the_owner_holds_only_what_the_daemon_tracks`,
  `a_put_below_the_floor_is_refused_after_deletes_too`); a whole channel has no floor, so its first message is held
  (`the_first_message_of_an_empty_channel_is_held`). A watch reply that is not `complete`, arriving during a read, outdates it
  (`a_pending_watch_reply_during_a_read_outdates_it`); a gap with no last try asks at once (`a_gap_with_no_last_try_asks_at_once`). A read's reply is applied only while a read equal to it is in flight
  (`a_second_reply_to_one_read_is_not_applied`), and the reply of a read a loss outdated replaces nothing
  (`the_late_reply_of_a_stale_read_leaves_the_state_needed`); fe-net drops a queued watch or read when its connection ends, so
  none is written on a later connection ahead of the owner's new watch, except one queued during the next connection's hello
  (`net/CLAUDE.md`). `watch` with the host and target already watched is
  nothing, and with the same host and channel it sends the watch alone (the daemon's watch replaces the connection's earlier
  one), an unwatch only for another channel or host (`watching_again_sends_only_what_changes`). `chat.changed` for the watched channel puts records and, for a record held, keeps that it was deleted
  (`changed_events_put_and_delete_the_watched_channels_records`): the owner holds each record with its deleted flag as one pair (`records()` returns the pairs), keeping only the deleted record's identity, author, time and origin, and the view passes the flag to the projection, which shows the header and `(message deleted)` alone (`a_deleted_message_is_its_header_and_the_notice_alone`, `a_deleted_record_shows_as_deleted_through_the_drawer`, `an_anchor_on_a_deleted_message_passes_on_when_a_snapshot_drops_it`). A deleted record counts against `KEPT`, a replacing snapshot holds none deleted, a put of the same time brings the message back (`a_put_after_a_delete_brings_the_message_back`), and a delete of a record not held adds nothing (`a_delete_leaves_a_tombstone_in_place`).
- A post is sent only when none waits for its reply (`a_post_is_sent_only_after_the_last_ones_reply`). One waiting when its link
  ends is `Unknown` (it may exist); one to a host that is not connected is never written and is `Refused(Unavailable)`
  (`a_post_is_unknown_only_when_it_may_exist`); a reply from another host answers nothing (`a_post_reply_from_another_host_answers_nothing`); each reply reads as it proves (`a_posts_outcome_reads_as_the_reply_proves`); a post fe-net refused locally (`ChatEvt::PostNotSent`, the connection's hello did not say the daemon posts plain text) was never written, so it reads as `Outdated` and frees the slot, and a notice for another post or from another host changes nothing (`a_post_not_sent_reads_as_outdated_and_frees_the_slot`). The
  owner sets the post in flight before State sends it, and `post` returns `None` both for a second post and a refused one.
- A status entry for another channel, a bare gap flag, or any status with nothing watched starts nothing
  (`a_status_entry_for_an_unwatched_channel_is_ignored`).
- The unsupported state is per host and clears when its link comes back (`the_unsupported_state_clears_when_the_connection_comes_back`).
`ChatDrawer` (`drawer/chat/drawer.rs`, State's `chat` field) privately holds the owner, a `RowChannel` and the reads in flight; the owner's
methods are `pub(super)`, so no other ui code can drop the requests they return. Rules:
- The window watches exactly the channel of the row the drawer shows, on that row's computer, while the chat is shown and
  nothing otherwise. The chat is shown when the drawer is Chat, or Help whose saved origin is Chat
  (`LayoutHelp::chat_shown`, `chat_is_shown_in_chat_and_in_help_opened_from_chat_only`); once the REPL opens over that Help the
  chat is not shown and the watch ends, though Help's saved origin survives, so a Help opened again over that REPL is watched
  while it shows. Each frame `tick_chat` asks `wanted_row` for the selected row's host and canonical id, never the slug, and
  does not resolve the selected row while the chat is hidden (`the_chat_follows_the_canonical_id_of_the_selected_slug`).
  The rest is pinned by `drawer::chat::row_channel_tests::the_watch_follows_the_shown_rows_channel_and_ends_with_it`.
- `RowChannel` resolves a wanted row once, with `chat.resolve`, after it has stayed the same for `NAV_FIRE_DEBOUNCE` on a connected
  host (until then the frame pacer is marked dirty); no answer outlives the wanted row or a link change, so showing Chat again resolves again. `RowChannel` keeps the last answer as a `Stand` (with `Unlisted` and `Offline` read from the wanted row and the links); `ChatDrawer::title` is ` chat · <channel id> ` and the words of the `Stand`, or for a mapped row of the owner's fill (`view_tests::every_outcome_has_a_state_a_next_action_and_words`, with each "asked again"); the words of a refusal code are wire's `ChatCode::text`, never the window's copy, except `unavailable`, which has the window's own words (not the hub; the hub's service not running); a target refused for good (`stale_row`, `stale_mapping`, `unmapped`) shows that text and is asked again only at a row or link change; a refusal that is neither the hub's nor the row's is asked again after `GAP_PACE`, its answer cleared meanwhile. The answer for the row
  still wanted decides its watch: `mapped` watches it (a second one is nothing, the owner takes the same target twice as
  nothing); anything else (`unmapped`, `invalid`, a refusal, an old daemon) ends the watch, so a channel the daemon no longer
  maps is not asked for again. An answer for a row no longer wanted is nothing. A row that changed or went ends the old watch at once, with the owner's `stop` and one `chat.unwatch`. A link coming
  up resolves the row again; a link going down resolves nothing (the row's state is reset then, which no test
  observes).
- Every `chat.history` goes through the set of reads in flight, per host. A read equal to one in flight waits, and goes when that
  one is answered (`a_read_from_before_a_rewatch_never_fills_the_new_watch`). A read whose reader gave it up stays as an
  orphan and its reply is dropped; a read still held when its reader gives it up is cancelled, never sent
  (`a_held_read_given_up_is_cancelled_and_never_sent`). The give-up is decided in one place, `owner_outs`, which compares the
  owner's read before and after each call it makes, so a watch stopped by hiding the chat (the first test), ended by a
  non-`mapped` answer or replaced by a `mapped` answer for another channel
  (`an_answer_that_ends_or_replaces_the_watch_orphans_its_read`) all give their read up the same way. A host's link change clears that host's set,
  since no reply comes for what its end dropped (`a_link_change_clears_the_reads_in_flight`; another host's reads
  stay, `another_hosts_link_change_keeps_this_hosts_reads_in_flight`), and, for the host whose channel is watched, is a
  give-up too: it cancels that host's held read, which never goes out later
  (`a_link_change_cancels_that_hosts_held_read`). A read the owner still holds
  is kept when a frame passes (`a_frame_during_the_read_keeps_it`). Limit: fe-net's
  exception (a request sent before the window has drained a link change can be written on the next connection) lets the
  clear admit an equal read beside it, and the earlier reply can then fill the new watch; the fix belongs to fe-net. A read held behind an orphan whose reply never comes waits until a link change; the daemon answers
  every history request but an admission hung on the shared home, which would hang a fresh read too.

Limits: for admitted or never-answered watches, a link that drops more often than every 26 s never shows a gap (a refused watch is a gap at once); records of the window's page below the daemon's own set (deletions between the daemon's read and the window's) are held and never updated; the `State` routing in `ui/events.rs` (`on_chat`, `on_chat_bus_lost`, `on_chat_connection`, `tick_chat`) is pinned by no test, since the tests drive `ChatDrawer` and its parts and a `State` needs a window; `tick_chat`'s own
choice of the shown row is among them (swapping `chat_shown()` for "the drawer is Chat", or never watching, passes the
suite); `Limited`'s `assumed` flag is not kept; a Slack socket outage shows in the title only when it costs a snapshot (a gap), since the daemon never says that a gap closed.

## Selection share
`SelectionShare` (`preview/share.rs`) keeps the daemon's published selection for a row equal to what the view shows selected: the preview's text selection or the row's drawing.
It privately holds the hosts that are up, the `Want` it last asked the daemon to hold (host, row and a `Share`: the text's node, range, `len` and
`digest`, or the drawing's base and `InkLayer::revision`) and the one `selection.set` in flight. It sends nothing itself: `reconcile` returns the request and State sends
it, from the frame's upkeep after `drain_events`, through the send `reconcile` is handed. The desired state is read from the
view each frame (`State::desired_text`, which gates on `preview_shows`, the predicate the paint and the press use, and
hands `wanted` its pieces; `step` adds the drawing): the selected row's released, non-empty `FlowedSelection` range over its installed preview, none
in edit mode or under the refused-hello overlay (`wanted_needs_every_piece`). The `Want` carries what the daemon checks:
start, end, `len` and the text's digest (`the_want_carries_what_the_daemon_checks`). The drawing is read from the owner itself (`wanted_ink` takes the `InkLayer` and the shown base, so base, strokes and revision are one drawing) and wins over the text; `step` holds every request while a stroke or an erase drag is live, so a drag sends once, at its end, and a clear or an undo to no strokes sends a clear, never an empty drawing (`the_drawing_is_wanted_from_the_owner_and_held_during_a_drag`). Rules, each with its test in `preview/share_tests.rs` unless named:
- A released range sends one request; an unchanged state, or a state while a request is in flight, sends nothing, and the
  latest desired state is sent at the reply, so changes in between are not sent (`the_window_shares_its_selection_and_withdraws_it`).
  There is no pending flag: the next frame's read of the view is the pending change.
- The range gone sends one clear to the row that held it, without a status line; a row switch is that clear, then the new
  row's share at its reply (same test).
- During a drag the view wants nothing, so a press that starts a drag clears a held share
  (`flowed_selection_tests.rs::the_released_range_is_ordered_and_absent_during_a_drag`).
- A reply earns one status line per kind (`shared`, `held`, `not_file_text`, `no_agent`, any other refusal, an old daemon).
  The state sent is what was asked, so a refusal is not retried until the state changes (`a_refusal_is_shown_and_not_retried`).
- fe-net does not replay the request. A failed send, an ended reply or a lost link marks the host down and forgets what it
  held (the daemon withdrew it at the connection's end); the state is sent again after `Connected`. A reply that does not
  answer the request in flight on that host is dropped (`a_lost_connection_is_answered_after_it_returns`). Hosts are kept
  apart: one's end or failed send leaves another's share and flight (`hosts_are_kept_apart`).
- A rendered markdown preview shares the whole file lines its selected text was rendered from, with the file's text
  there: the walk marks each paragraph's, heading's and code block's spans with their file lines (the front matter's
  lines added), `SourceLines` (`preview/markdown/source_lines.rs`) maps each buffer line's bytes to those lines, and
  `released_selection` names the least to the greatest line of every text run the selection covers a byte of, through
  `sot_protocol::ops::whole_lines`. A paragraph shares its block's lines (comrak's inline positions are not reliable), a
  code line its own, a bullet, gutter, rule, table, equation or figure none; a paragraph after link reference
  definitions shares theirs too. A map is built only for the preview pane's shape of a project file (`maps_source`) and
  kept only when it lines up with the buffer byte for byte; a rendered buffer without one shares nothing, and never its
  buffer positions. Tests `markdown/source_lines_tests.rs`: `a_block_of_prose_maps_to_its_file_lines`,
  `each_code_line_maps_to_its_own_file_line`, `decorations_and_media_hold_no_line_and_a_range_across_them_covers_them`,
  `the_map_is_kept_only_when_it_lines_up_with_the_buffer`; `shaped_tests.rs`:
  `a_rendered_markdown_selection_is_shared_as_the_files_own_lines`, `a_rendered_buffer_with_no_map_shares_nothing`. A
  math, figure or `markdown.tokenize` reply reflows a markdown preview and clears its selection a frame after the share
  was sent (SELECTION-WITHDRAWN-ON-REFRESH).
- Limits: a clear made while disconnected relies on the daemon's withdrawal at the connection's end (for a half-open peer
  that waits for its ping deadline); a reply is matched to the request in flight by content, not id, which is sound only
  because fe-net emits every `Ended` of a session before its `Disconnected` and the next `Connected`; the selected text
  and its digest are rebuilt each frame, and a release with no later event waits for the next frame; only the preview's flowed text and the row's drawing are shared (not the agent pane, chat or concept pane); a re-rendered figure on the same node does not raise the drawing's revision, so it reaches the agent's picture at the next share; a text selection replaced in place by a drawing stays shared on a daemon that takes no drawing (fe-net answers the drawing unsupported and writes nothing); a row with no
  workspace id (the daemon's default row) is never shared; the daemon's own check decides whether the text is the file's;
  the preview pane's call that turns the source-line map on for rendered markdown (`maps_source` true in
  `preview/pane.rs`) is pinned by no test, nor is the arm that leaves a map that does not line up unshared
  (docs/defects.md: SELECT-MARKDOWN-PINS).

## Folders
- `app/`: the winit application (`App`), event-loop callbacks, one frame (`frame.rs`) and the quit prompt and exit.
- `render/`: glyph text, the ratatui cell backend, textured quads, the surface and the capture readback.
- `render/pass/`: the frame's render pass, one `State` method per section, called in order by `State::redraw`.
- `chrome/`: pane geometry and focus delegation, the status line, nav spill, colours, the wireframe and the frame's chrome draw.
- `chrome/strip/`: the session strip band under the panes.
- `input/`: FocusPrompts, LayoutHelp, the action catalog, key and mouse routing, help and clipboard paste.
- `nav/`: the navigation pane's trees: `Navigation` (active tree, mode, scroll, store), `TreeView`, `NavCursor`, the
  Modules, Sessions and Hosts trees.
- `nav/files/`: Files mode's prompts, reveal walk, transfers and re-lists.
- `session/`: `RowDirectory` owns listings/identity, `RowPresentation` owns metadata/order/flashes, `ResultLedger`
  owns owed results/attempts, `ParkedViews` the parked row views and `SelectedRow` the selected row and its read
  mark; State coordinates effects, snapshot capture and restore, the switch, picker, presence and the badge floor.
- `agent_pane/`: `AgentPane` and its attach, input, screen and reply operations; thin State adapters apply effects.
- `drawer/`: the bottom drawer and its tenants.
- `drawer/repl/`: ReplModel owns row behavior; ReplPresentation owns image refresh, projection, layout, anchoring
  and painting; thin State adapters apply transport and presentation effects.
- `drawer/chat/`: `owner.rs` holds `ChatOwner`, the project chat's private owner (fields private, no open mutation; its tests are `owner_tests.rs`); `row_channel.rs` holds `RowChannel`, which
  resolves the wanted row; `drawer.rs` holds `ChatDrawer`, which owns both and the set of reads in flight, and is what State holds; `mod.rs` holds State's side
  only. State forwards chat events, `bus.lost` and each link's change to `ChatDrawer`, whose methods return the requests to
  send. `project.rs` turns a record into kinded spans (`project`, pure; `project_tests::chat_markup_and_hostile_text_remain_literal`):
  the text goes through wire's `text_segments`, Slack's markup is its own `Markup` span shown decoded (entities read, nothing rendered), CR LF and CR become LF, tabs stay, any other C0 or C1
  control becomes U+FFFD, and a name or file name loses exactly the C0 and C1 controls and the characters `page_reply::unsafe_char` lists (line separators, direction and zero-width formatting characters, U+2060 to U+2069, U+FEFF, U+FFF9 to U+FFFB, U+FFFE and U+FFFF), so no body line can read as a header (the header ends its own line). The time is added to the stamp as integers (`HH:MM` for today's local date, else with the date; a stamp out of chrono's range shows as its own text, never a panic).
  Limits: one fixed offset serves every stamp, so a post from before a daylight saving change shows an hour off; a member whose name ends in `[drawer]` reads like the drawer's own post but for the span style; a name made only of removed characters shows empty (a file name so made shows `[a file]`); what stays in a name includes the soft hyphen, tag characters, U+180E and U+206A to U+206F, none of which breaks the header line.
  `view.rs` is the view (`ChatView`, with `ChatDrawer::layout` and `areas`): `place` puts the records in the drawer's body, upward from the newest at the
  bottom while following (the anchor is `None`) and downward from the anchor otherwise, and asks a record's height only when that record reaches the body. A record is shaped by
  `set_buffer_rich_text` from `project`'s spans, each kind with its own style (`attrs`: the header's differs from the body's, so a body line that imitates a header looks like a body),
  and its buffer is cached by those spans, the width and the text scale; the buffers in view are kept plus up to 64 others, the farthest from the position first. The records
  that cross the body are prepared as clipped `ExtraArea`s appended to the frame's one `extras` vector, before its one `prepare_text`. `no messages` is drawn
  only when the owner's fill is `Complete` and it holds no record (`ChromeView::chat_empty`, `ChatOwner::no_messages`; not while reading, offline, in a gap or unsupported:
  `view_tests::no_messages_shows_only_for_a_channel_read_whole_and_empty`; the line is drawn only while Chat is shown and `chat_empty` is set, `chrome::view::tests::the_chat_drawer_draws_no_messages_only_when_told_the_channel_is_empty`; `draw_chrome`'s passing the flag from State is unpinned, as a State exists only in a native window). The order is pinned by `view_tests::the_view_shows_records_oldest_to_newest_from_an_owner_whose_records_arrive_out_of_order`, the cache's farthest-first bound and the top clip by
  `the_view_shows_the_held_records_newest_at_the_bottom_and_shapes_only_what_it_shows`. A reader scrolled up stays put: the anchor is `None` (following the newest) or `(ts, dy)`, the top record's time and its top `dy` below the body's top, so an arrival, an edit that changes any record's height, or a delete elsewhere moves nothing (but see the limits). PageUp and PageDown (`ChatView::scroll`, `Step::Page`) move a third of the body, Alt+Up, Alt+Down and the wheel one line (`LineScroll::Chat`, `chat_wheel`), and `chat.latest` (End, `Scope::Chat`) sets `None`; they act only while the drawer is Chat (`chat_move`, `chat_wheel`), the REPL's Alt+Up and Alt+Down unchanged. Each frame finds the anchor by its time among the held records (an anchor no longer held passes to the nearest newer record at the same `dy`, so a snapshot that replaced every record does too, and with none newer the view follows), then `settle` takes up the records above the top while there is room, drops those wholly above it and makes a position whose newest record is wholly in view `None` (`view_tests::an_anchored_reader_stays_put_through_arrivals_edits_and_deletes`, `the_keys_and_the_wheel_reach_the_view_only_in_the_chat_tenant`). A row change returns the reader to following (`a_row_change_returns_the_reader_to_the_newest`). Keys and the wheel are kept as line and page counts and turned into pixels by the frame that spends them. Alt+Up and Alt+Down reach the chat through `line_scroll_target` (focus in the drawer, Chat shown). Limits: at the 100-record bound each arrival evicts the oldest, so a reader on the oldest held message moves up one message per arrival until older pages are read; a delete or an edit that brings the newest wholly into view returns the reader to following, which can move the text by at most the height lost; a record taller than the body shows its end.
  `compose.rs` holds the draft (`Compose`, one `Draft` per canonical row; `compose_tests::the_draft_keeps_its_destination_and_its_escaped_bound`). It holds only what wire's `post_text_fits` admits (the escaped form within 8 KiB, so the post reads back whole): a typed key or a paste past it is refused whole, with a status line, and the draft is as it was (an oversized first paste begins none). Its destination (target; the thread comes with B5p) is the channel watched at the first character and kept through any change of view; an emptied draft is dropped and follows the view again. Typing and pasting make CR LF and CR into LF, keep tab and drop every other control character (C0, DEL and C1), which is exactly what the drawer cannot show. `draft_edit` says which key edits it: a typed character or Space only where `chord_types_text` (`input/mod.rs`, also the editor's) says the chord types (AltGr does, Ctrl+a does not) and only when the key resolved to no action, Backspace (also with Ctrl) deleting one character, and `chat.newline` (Shift+Enter); plain Enter does nothing until B5m. In the Chat tenant (`DrawerInput::Chat`) `chat_key` sends a key or a paste to `ChatDrawer::edit`, which is given the row the chat shows at that key (`wanted_row`), not the last frame's, and begins a draft only in the watched channel if that watch is that row's (`compose_tests::the_wired_draft_is_the_shown_rows_and_begins_in_its_own_watch`); a refusal goes to the status line. A listing's retired rows drop their drafts (`ChatDrawer::retire`, called from `reconcile_pending_results`). The compose rows sit at the foot of the drawer (`compose_rows`, over `ComposeText` that `ChatDrawer::compose_text` builds once a frame and only while Chat is shown): `> ` and the draft's last three rows, each line of the draft (through `cell_lines`) wrapped to the drawer's width with `wrap_status`, a cursor block ending the last while the drawer has focus, then `to <channel>` with how many rows are not shown, so at most four; in a drawer shorter than that it keeps one draft row fewer than its height, counting the rows it dropped, and `to <channel>` is the last dropped; none while no channel is known (`compose_tests::the_compose_rows_show_the_draft_wrapped_to_the_drawer_and_counted`, which also asserts the draft's exact text; `chrome::view::tests::the_chat_drawer_draws_its_compose_rows_at_its_foot_and_no_other_drawer_does`). The chrome draws them and reports their number (`ChromeView::chat_foot`, kept by `ChatDrawer::set_foot`), and the records' body is that many rows shorter (`layout_drawer_px`). Limits, each needing a window: the keys hook's paste branch (the clipboard read), `chat_key`'s `last_key` and `edit_chat_draft`'s status line, the call from the listing to `retire`, the editor's call of `chord_types_text`, the records' body being shorter by the compose rows (`layout_drawer_px`), the cursor block shown only while the drawer has focus, and the compose text built only while Chat is shown (both in `draw_chrome`); the draft is kept in memory only.
- `drawer/terminal/`: the Terminal tenant (local pty, vt100 helpers).
- `preview/`: PreviewRequests owns request clocks, the fired target, concept follow, driven holds and the crop,
  save and aim transactions; InstalledPreviewSource owns the installed source; State coordinates preview installation,
  shaping and transport (fetch, replies, layout, concept, open).
- `preview/image/`: bitmap previews as quads: figures, overlays, view, ROI.
- `preview/markdown/`: markdown and source text shaped into cosmic-text buffers; `source_lines.rs` maps a rendered buffer's text to the file lines it came from.
- `preview/editor/`: the in-pane edit buffer.
- `control/`: the fe.command route, its dispatch, the reply bar (`reply_bar.rs`, with State's side in `reply_dispatch.rs`),
  the nav envelope and the file channel.
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
- `input/`: FocusPrompts, LayoutHelp, their behavior tests, key bindings, help rendering and paste (its own page).
- `nav/`: the navigation pane's trees (its own page).
- `persist/`: settings, discovery and resume state (its own page).
- `preview/`: the preview pane (its own page).
- `render/`: the window's pixels (its own page).
- `session/`: which session row the window is on (its own page).

## Start here
`State` in mod.rs for view state; `app/handler.rs` for the event loop; `app/frame.rs` for one frame; `events.rs` for a
wire reply (then the `on_<variant>` it names); `input/keypress.rs` for a key. Each folder's page says its own rules.
