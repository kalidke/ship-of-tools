# rust/frontend/src/ui: fe-ui, the window (charter)

## Idea
One UI thread owns all view state and every other thread reaches it only by waking the window; ratatui computes cells
and one wgpu pass draws every pixel. The window's `State` (mod.rs) is driven by one winit loop. `session/row_directory.rs`
owns authoritative rows and identity; `session/row_presentation.rs` owns their presentation;
`session/result_ledger.rs` owns owed results and their attempts; `agent_pane::AgentPane` owns the selected agent pane.
`drawer/repl::ReplModel` owns REPL row state, submissions, history, routing and lifecycle;
`drawer/repl/presentation::ReplPresentation` owns window-wide REPL images, layout and scroll anchoring.
`input::FocusPrompts` owns window-wide focus, nav prompts, captured scale focus, stayed notice and quit hint.
`input::LayoutHelp` owns drawer selection (the shown row's; each row's is parked in its view), maximize/wide flags, Help browsing, return origin and startup requests.
`preview::requests::PreviewRequests` owns both request clocks, the fired preview target and its page transport, concept
follow commitment, the driven-preview hold, and the crop, scale-save and `--roi` aim transactions.
`preview::source::InstalledPreviewSource` owns the installed preview bytes, their node, scale and markdown origin.
`session::ParkedViews` owns the rows' parked views (`WorkspaceUiSnapshot`, each with its row's drawer), keyed by row.
`session::SelectedRow` owns the selected `(host, workspace)`, its read-mark dwell, the destroy confirm and a create's
request time; `session::WorkspacePicker` owns the new-session picker's listing, cursor and account choice.
`nav::NavCursor` owns the nav cursor's settle debounce, startup/relaunch/reconnect placements, README default, pin and
project-scan clock; `nav::TreeView` owns a tree's rows, cursor and content generation; `nav::Navigation` owns the
active tree, its mode, scroll and scan root, and the store of parked trees; `nav::RevealWalk` owns the reveal walk's
target, awaited level, refresh memo and root-armed target; `nav::FileOps` owns the pending create and delete and the
upload batch. `preview::concept::DriftCheck` owns the drift badge's per-file parsed hashes, parse latch and backoff;
`preview::editor::state::Editor` owns the active edit, the file edit awaiting its `file.read` and the shaped edit buffer;
`preview::concept::ConceptSlot` owns the last annotation read, its shaped body and the concept pane's rect;
`flowed_selection::FlowedSelection` owns mouse selection over flowed text; `motion::ScrollMotion` owns the rule of a person's eased scroll (the owed distance, its deadline, the last frame's time and a cell pane's carry of rows; a pane keeps its own position and edges); `preview::shaped::ShapedPreview` owns the flowed preview buffer, its rect, scroll and definition-line
anchor, the refused-hello overlay and the highlighter; `preview::image::raster::RasterView` owns the shown raster, its
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
`preview::image::ink_input` wires it (D4c1): a left press inside the image rectangle with drawing on over the shown base (`State::shown_ink_base`, which calls `InkLayer::shown_base`; no second decider) focuses the preview first, begins a stroke and starts no text selection but still clears the agent pane's selection (`SelectionContext::draw_press`, first in `press_selections`; `a_press_the_drawing_takes_focuses_the_preview_and_starts_no_selection`); the layer owns that routing (`left_press`, `moved`, `right`, `focus_lost`; `the_window_routes_a_left_press_motion_and_the_right_button_through_the_layer`): motion extends a live stroke, else a right drag erases, release commits, the window losing focus commits a live stroke and ends a held erase; a right press goes through the quit question and the stayed notice as a left press does and a release of either button never does (`a_right_press_answers_a_question_or_a_notice_and_a_right_release_always_reaches_the_eraser`); `sync` keeps a live stroke over a size change (`a_size_change_keeps_the_live_stroke_and_paints_it`) and drops it with its base (`a_base_no_longer_shown_drops_the_live_stroke_and_the_same_base_stays`), and a size change repaints the raster (`the_same_node_at_another_size_rebuilds_the_raster_at_once`); `preview.draw` (`d`, Image scope), `preview.ink_undo` (`u`, `Primary+z`) and `preview.ink_clear` (`Delete`) in the `Drawing` scope, live in the focused, not editing preview while drawing is on over a shown base (`Context::drawing`), resolve only there (R-D15, `the_drawing_keys_resolve_only_where_drawing_is`); the status line says `drawing on: ...`, `drawing off`, `drawing cleared: u brings it back`, `drawing: no image to draw on` (under the refused-hello overlay), `drawing: only image files can be drawn on; open a PNG of it, or press <canvas label> for a blank canvas` (over a shown picture that is no image file), and the layer's own `the drawing on <name> was replaced` and `the drawing is full: undo or clear`;
limits: the draw order, the scissor and the texture upload are in the render pass and no pure test reaches them, and neither does the State routing: the node `RasterView` records at install (cleared with the quad at hide) and the argument the gate reads are unpinned, so a slow or failed reply or a failed decode keeping one figure's ink off another's rests on reading, and `write_region`'s transcription of origin, extent and row length into `write_texture`, and the overlay's resize and its dirty rectangle's clearing in `upload`, are unpinned; the window repaints the whole raster on the UI thread (the wire's `mask`, 36 to 243 ms at the point bound for a hand-drawn drawing on a 2000 x 2000 raster, 620 ms for 300 px steps, measured at the earlier 1/200 ink width; the 1/300 width visits about a quarter fewer pixels, so these are upper bounds), once per undo, clear, erase hit or new base and each return to a row whose drawing's base is shown; the canvas's white rectangle and its overlay (`paint_canvas`) are in the render pass and unpinned like the figure's; unpinned in D4c1: the State handlers (`press_ink`, `ink_moved`, `ink_released`, `ink_right`, the focus loss, the key handler) the render pass's `ink_sync` and `set_frame`, and the press handler's setting of focus, which rest on reading, as does `sync`'s call from each handler; D4c2: the row's drawing rides its parked view (`WorkspaceUiSnapshot::ink`, the layer's own `Drawing`; `park` and `restore`): leaving a row parks it, entering restores it (a first visit restores the default, so the layer starts clean), a destroyed row's view drops it with the rest; a restore keeps nothing else of the row left: no live stroke, held button, frame, raster or overlay (`sync` repaints the raster at the next frame), and raises `revision` (`a_parked_drawing_keeps_the_cleared_copy_and_the_undo_history`, `a_switch_drops_the_live_stroke_the_held_button_and_the_raster_and_raises_the_revision`, `a_restored_drawing_keeps_its_own_mode`, `a_rows_drawing_comes_back_with_its_view`); the State's three calls (the park, the first-visit restore, the return's restore) and the destroy's order are pinned by the native fixture `a_rows_drawing_stays_with_its_row`, which runs only under a display; a window's close, and a restart, lose every row's drawing (nothing persists it); the canvas's State wiring rests on reading: `preview_shows` taking `shown_base` as its canvas, `ink_src` handing `source_dims` the served size, the drawer's `canvas` and `drawing` fields, the Shift+d handler's refusal under the overlay, and the status lines `canvas: draw with the pointer; u undo, Delete clear, Shift+D back` and `canvas hidden`; not built: a source past 6000 px and over 20 MB (`PREVIEW_DOWNSAMPLE_TRIGGER`) is served downsampled and the preview reply carries no source size (`files/preview/mod.rs` folds the factor into `physical_scale` alone), so the raster is `raster_dims` of the served size and differs from the daemon's for such a source (the D4b review's N3)
`preview::image::figure_store::FigureStore` owns the markdown figures' bitmaps, requests in flight and failures;
`preview::image::svg_preview::SvgPreview` owns the SVG file preview's two kept outcomes and its one job in flight;
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
  fe-command watchers, the drawer terminal's reader and the `FeAttachClient` wake closures (agent_pane/attach.rs), and the SVG worker
  (preview/image/svg_draws.rs), which wakes after each outcome it queues. `State`'s window is a `MainWindow`, which is `!Send`, so no other thread holds it, and `create_window` returns it
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
  bytes; `figure_failed` stays with the markdown media and travels beside it in the snapshot. A row's restore brings
  back its fired target only when the parked bytes answered it or the daemon refused it: a target whose reply came
  after the row was left (and was dropped) is forgotten, so the cursor's follow asks for it again. A return before the
  reply costs one more fetch of the same file, bounded, never a loop (the reply that was in flight may still be
  admitted, and the follow then asks once more). The switch empties the pane's flowed text (`clear_preview_text`) before
  every restore, so a row with no installed bytes, or bytes that fail to render, never shows the row left under its
  title. Pinned by the native fixture `a_row_left_before_its_preview_came_back_shows_its_own`: an answered row and a
  refused row returned to several times ask nothing, an unanswered row asks once per return and stops once answered.
  It does not reach the bounce within the reply's window.
- `ParkedViews` privately owns the map of parked row views. Leaving a row parks its view (replacing an earlier one),
  coming back reads a clone, a kept default row's end forgets only the parked attachment, a destroyed row's view is
  dropped, and the default row's raw-slug key migrates to `<default>` with an existing `<default>` view winning. State
  composes and applies the view; the map is reached through nothing else. A view holds its row's drawer (closed, or the
  tenant shown; never Help): `snapshot_current_workspace_ui` parks `LayoutHelp::row_drawer`, a return applies it through
  `LayoutHelp::enter_row_drawer`, and a first visit is closed. The row shown at start has no switch behind it: its drawer
  is whatever the window started with (closed, the Terminal after a relaunch, the Monitor under `--start-monitor`) and
  is parked when the row is first left. The one rare case: a relaunch whose resumed row is gone at the first list moves
  to a neighbor as a first visit, so the Terminal closes there. The drawer's height, maximize and wide preview stay
  window-wide, and the parked drawers are runtime memory: a window restart starts every row closed but the one shown.
  Pinned by the native fixture `a_rows_drawer_stays_with_its_row`.
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
  (resized only when the rect moved by more than half a pixel), the scroll in body lines, the
  definition-line anchor, the `ScrollMotion` that spends a person's scroll over frames, the refused-hello overlay and a `FlowedSelection`. Its `hit`,
  `press`, `extend_drag`, `release`, `clear_selection`, `selected_text` and pure `selection_rects` operations lend the
  selection the text origin, top padding, scroll and media placeholder; the render pass tints the clipped rectangles before painting glyphs.
  Installing a buffer clears selection and scrolls to the top. A row switch, Escape in the preview
  and opening edit mode also clear selection. `released_selection` answers the file's text: a plain or token buffer's
  own range and text, rendered markdown's source lines whole (`TextOrigin::Rendered`), and nothing for a rendered buffer
  with no map (`TextOrigin::Unshared`). The text scope's `preview.copy_selection` copies rendered text on
  Primary+C or Primary+Shift+C; `y` still copies fenced code sources. Selection does not cover MarkdownMedia's wide
  table buffers, math images, image previews, the concept pane or double-click word selection. A
  sent `preview.get` arms its anchor; the reply consumes it and scrolls to the definition only for a code reply to the
  request that armed it; a cursor move within the shown file re-anchors only on a real change of line. Scrolling is
  named: by a signed step stopping at the top, top, end, each an ask at the event's time and a length (`[display]
  smooth_scroll_ms`, 0 to 1000, default 150; zero lands at once), never a move. The scroll is fractional body lines, whole at rest;
  `advance`, run by the layout's clamp each frame, resolves an End against that layout's end, spends the owed distance
  (a person's travel past one pane height at once), stops it at the top or the end, lands a rest on a whole line and
  answers whether frames must keep coming. A new file, a reply's anchor and a change of the drawn buffer (edit mode
  opening or closing) move at once and owe nothing; a cursor move's re-anchor and Home and End ease. The drawn offset
  (`scroll_px`, which the hit test, the selection and every paint read) is on a whole physical pixel between lines
  (`a_wheel_moves_the_preview_over_frames_and_its_hit_test_follows`, `home_and_end_ask_their_whole_distance_resolved_at_the_layouts_end`,
  `a_new_file_and_a_change_of_buffer_end_what_is_owed`, `a_zero_length_scroll_lands_at_once_and_an_edge_drops_what_is_owed`,
  `a_reanchor_inside_the_file_eases_and_a_reply_anchor_lands_at_once`, `an_edge_stops_the_motion_at_the_frame_that_meets_it`,
  `a_reanchor_after_a_wheel_step_lands_on_its_line_not_beside_it`, `the_last_ask_wins_over_an_end_not_yet_resolved`). Every
  call site asks with `State::scroll_ask` (the event's instant and the one length, `settings.smooth_scroll_ms`), so the length is read in one place; a harness instance
  (`ephemeral`, any `--capture`) has it set to 0 at start by `Settings::for_harness` (`a_harness_instance_asks_with_zero`), because its idle
  loop returns before the pacer can schedule the next frame, and the native fixture, whose window is such an instance, sets its own 150 ms.
  The State glue is pinned only for the wheel: the native fixture `a_preview_scroll_moves_over_frames_and_comes_to_rest`
  (which runs only under a display) drives the wheel's call site and `clamp_preview_scroll`'s `mark_dirty`. The keys'
  eight call sites in `preview/keys.rs`, Alt+Up and Alt+Down in `input/global_keys.rs` and the re-anchor in
  `preview/fetch.rs` rest on reading, like the other State handlers.
- `ScrollMotion` (`motion.rs`, a leaf module with no `impl State`) privately owns the owed distance, its deadline and the
  last spend's time. `ask` adds a distance and restarts the deadline from the ask's time (a zero length answers what to
  apply at once); `spend` keeps `owed * ((deadline - now) / (deadline - last))^3`, so the sum of every spend is the asked
  total and the last of it lands `length` after the last ask, spends all of it at the deadline, and spends at once
  whatever is owed past the pane height it is given; `finish` and `stop` end it. It knows no pane, edge or unit; the
  strip's motion stays `StripMotion`. Behavior tests: `motion_tests` (`an_ask_comes_to_rest_exactly_its_length_after_it_on_the_asked_total`,
  `the_first_frame_already_moves_and_the_steps_shrink`, `the_curve_is_a_cubic_ease_out`, `a_second_ask_continues_from_where_the_view_is_and_restarts_the_length`,
  `what_is_owed_past_a_pane_is_spent_at_once_at_the_next_frame`, `a_zero_length_applies_at_once_and_owes_nothing`,
  `finish_and_stop_end_the_motion_and_an_ask_that_cancels_owes_nothing`, `a_frame_at_or_before_the_ask_moves_nothing_and_a_late_one_rests`).
  Whole rows (`ask_rows`, `spend_rows`, `finish_rows`, for the cell panes) add the one field it keeps for them, the carry:
  the distance spent minus the rows shown. A frame shows the spent distance rounded up in the direction of travel, so the
  first row shows at the first frame and a one-row step is that row; the frame that rests rounds, so the rows shown
  total the rows asked (`whole_rows_move_the_first_row_at_once_and_end_with_the_last`,
  `a_zero_length_in_rows_applies_at_once_and_stop_drops_the_carry`). The preview, the agent pane and the Terminal drawer
  use it; a pane's ask, spend and edges are that pane's.
- `RasterView` privately owns the image pane's view of one raster: its texture, loaded and served dimensions, zoom,
  pan, the visible source-pixel ROI, the per-directory, per-size view carry and the carry parked at install. A decoded
  raster installs; unless a page re-raster keeps the view, the view resets to fit and an image node's cached
  neighbor view is parked. Each frame clears the ROI and, with a raster shown, frames the canvas: a ready `--roi`
  aim wins over the parked carry, a degenerate solve is logged and dropped, then zoom and pan are clamped to the live
  geometry. State computes the ROI for an image node, which the owner records and writes through to the carry
  outside the quantization window; State echoes an applied aim and draws the quad. Keys reset, pan and step the
  zoom (1.25x below 1.5, whole steps above, pan scaled to keep the center).
  An SVG file (`image/svg+xml`, `preview_route`'s `Svg`) is drawn by the worker into the same `RasterView`, never on the
  window's thread: `show_svg_preview` asks `SvgPreview` (preview/image/svg_preview.rs) for a `Fit` job (the pane's size
  in physical pixels as of the last layout (`PaneRects::preview_px`, the cells times that frame's cell size, so a read
  between a text-scale change and the next layout is the pane's own), `NO_PANE` (1024 square) before a layout, and the
  GPU's texture limit), the child works the size out from the file's own (`svg_fit_dims`: the SVG's aspect, the pane's limiting side,
  no side past the limit and no more than `MAX_RASTER_PIXELS` in all), and the runner refuses a reply past either
  limit at its header (`Ask::accepts`). Until the picture is drawn the SVG's text is shown, as any text file's; then
  `apply_svg_step` uploads the picture (filtered Linear, view at fit, no carry) into the `RasterView`. `SvgPreview`
  keeps the last two outcomes, each with a digest of the file's bytes (SHA-256 and the length), never the bytes: a
  drawn picture with the ask it was drawn for, a refusal for the file alone. An equal preview (equal digest and ask,
  whatever the generation) is installed from a picture with no child, so a switch between two rows that each show an
  SVG draws neither again; a refused file costs one child, not one per return or per size. While a picture of the
  shown source is installed, a render of that source at another size (a text-scale change, a layout that moved the
  pane) leaves it up (`Step::Held`) until the new picture replaces it: the SVG's text never replaces a picture. One job is in flight,
  keyed `PREVIEW_KEY` (a key no figure URL holds), of the pane's generation; a second `show` of the same generation
  while it is in flight submits nothing, and an outcome of another generation (a departed source's `Stale` or a late
  outcome after a resubmit) is ignored. The frame's one step, `settle_frame`, tells the worker the pane's generation,
  drains it once and hands each outcome to its owner (the preview by `PREVIEW_KEY`, a math snippet by its key's prefix to `MarkdownMedia::settle_drawing`, else `FigureStore`); the window keeps the
  upload (`settle_svg_draws`, called by `frame_upkeep`, and `apply_svg_step`). A refusal says `svg not drawn: <the
  drawing's or the runner's first line>` on the status line (`svg_not_drawn`, no control character) and the text
  stays, the old raster hidden. A refusal that arrives while a picture of the same source is up leaves the picture
  with the line until that source is next rendered; that render finds the refusal kept, and the picture goes for the
  file's text and the line. A refusal is the file's alone, so one bound's end (`Ended`, such as `took past 10 s`, which
  depends on the size asked) refuses the file at every size until it leaves the two kept slots. A file that is not UTF-8
  shows one line saying so, never an earlier file's text or picture: `render_preview_source` checks UTF-8 once, before
  markdown and plain text take their text, so every mime reaches it (a `tokens+json` that fails to decode installs
  nothing, as before). An SVG is not an image node
  (`is_image_node_id`): it has no ROI, crop (`c` says an SVG cannot be cropped), scale bar, caption or drawing, since
  the daemon can decode none of them (drawing over an SVG is not built: `d` over any shown picture that is not an image file says the line
  `preview::image::ink_input` names, `drawing_over_a_picture_that_is_not_an_image_file_names_the_two_ways_to_draw`; over the SVG's text `d` resolves to
  no action, as over any text file, so it says nothing there: the route that would say it is about 10 lines, left because the state is
  transient or the refusal's own status line explains it; the key's reach and the label's action rest on reading, the test pins the line's choice and words).
  Limits: a zoom past fit enlarges the raster's pixels (no re-raster at zoom); a pane resized after the draw scales the
  picture until the preview is next rendered; a text-scale change shows the kept picture when the pane's pixel size is unchanged and otherwise draws
  again with the old picture up; a row switch back shows the kept picture (view at fit, no carry); the window's heap
  holds up to two kept pictures of at most 64 MiB each (RGBA, 16 Mpx), and no kept file bytes; each submit copies the
  file's bytes once for the job (up to `PREVIEW_BINARY_CAP`, 512 MiB, for a file the daemon serves as `image/svg+xml`),
  which the worker holds until its draw ends, and the digest reads every byte once per source on the window's thread
  (SHA-256, not timed); `.svgz` is not served as `image/svg+xml` by the daemon (`mime_for_path`) so it is
  not drawn; an SVG over `PREVIEW_TEXT_CAP` shows the large-file line, not its text, until its picture is drawn. Tests:
  `a_preview_is_routed_by_its_mime_an_svg_to_the_picture_not_the_raster_decoder_or_the_text`,
  `the_svg_raster_fits_the_pane_keeps_its_aspect_and_stays_under_the_texture_limit`,
  `the_svg_raster_is_bounded_by_area_and_a_huge_declared_size_draws_small`,
  `an_svg_that_will_not_parse_says_why_for_the_status_line`, `an_svg_is_not_an_image_node`,
  `a_capture_of_a_shown_svg_says_it_cannot_be_cropped`, and in svg_preview_tests.rs (a stand-in draw)
  `a_current_preview_installs_its_raster_and_a_stale_one_does_not`,
  `an_equal_preview_is_installed_from_the_cache_without_a_draw`, `two_outcomes_are_kept_and_the_oldest_goes`,
  `a_refused_preview_is_not_drawn_again_on_return`, `a_refused_file_is_not_drawn_again_at_another_size`,
  `a_picture_stays_up_while_its_redraw_is_with_the_worker_and_never_across_sources`,
  `a_second_request_of_one_generation_starts_no_second_job`, `files_of_one_length_are_not_taken_for_each_other`,
  `a_late_outcome_of_a_departed_generation_does_not_undo_the_job_in_flight`,
  `a_figure_and_the_preview_each_settle_through_the_one_drain` and
  `a_preview_the_worker_will_not_take_is_refused_at_once`; the child's fit ask in tests/svg_child.rs
  (`a_fit_ask_is_drawn_at_the_size_the_child_works_out`, `the_runner_returns_a_fit_ask_at_the_childs_size`,
  `a_fit_reply_past_its_cap_or_the_pixel_budget_is_refused_at_its_header`); and the native fixture
  `an_svg_preview_is_a_raster_and_a_broken_one_is_its_text` (through the real child, the frame's own `frame_upkeep`:
  after a layout, the text shows until the picture is installed at the pane's size, the same file again is installed at
  once, a text-size change leaves the picture up, a file that is not UTF-8 says so, one per route (`text/plain`, `text/markdown`, `image/svg+xml`), each right after a
  picture over an earlier file's words, a broken one says why).
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
  settles its URL, then installs or fails it; an unresolvable local URL fails at once. An SVG reply is the exception:
  `route_reply` never draws it on the window's thread but submits one job to the drawing worker, keyed by its URL under
  the pane's generation, and the URL stays pending until the outcome; the window decodes only what `route_reply` hands
  back (`Reply::Raster`). Each frame `svg_preview::settle_frame` tells the worker the pane's generation and hands every outcome it
  returned to its owner (a figure's to `settle_drawing`), and the window (`on_svg_settled`) uploads a picture or reflows. Drawn, it is installed onto 800 by 600
  pixels (`FIGURE_SVG_SIZE`); refused (the worker's words, a bound's end, a full queue or no worker thread), it fails
  like an undecodable figure, terminally until a fresh markdown reply; stale, it leaves `pending` and one reflow lets
  the walk fetch it again under the new generation. Only the outcome of the generation a URL's job was submitted under
  counts: a departed generation's `Stale` can arrive after the new one submitted the same URL, and is ignored. Tests
  (preview/image/figures_tests.rs, a stand-in draw): `a_figure_stays_pending_until_its_drawing_settles`,
  `a_raster_reply_settles_its_url_and_comes_back_to_decode`,
  `a_refused_figure_is_not_drawn_again_until_a_fresh_reply`, `no_figure_url_can_hold_the_previews_key`,
  `a_stale_figure_is_fetched_again_under_the_new_generation`,
  `a_late_stale_after_a_resubmit_does_not_undo_the_pending_job` and `a_figure_the_worker_will_not_take_fails_at_once`.
  Limit: the window's two calls to the store (`on_figure_loaded`, `frame_upkeep`) and the upload need a GPU and have no
  test.
  Failures report to the walk as zero-size metrics, are cleared only by a fresh markdown reply or a first visit, and a
  row parks and restores its own.
- `MarkdownMedia` privately owns the rest of the markdown media. Math and token requests go out only for keys
  neither cached nor in flight; a reply caches, settles its key and asks for one reflow (a math failure settles with
  no reflow, so it cannot loop); a figure's landing or failure asks for one too, and the frame takes it once. Table
  buffers are rebuilt only when their sources change, which also returns the shared horizontal scroll to the left;
  the scroll is clamped each frame to the widest table. A math SVG is drawn by the worker, never on the window's
  thread: the paint pass (`prepare_media`) collects the shown snippets that want a raster (`MathSvg::wants_raster`: not
  drawn, and not asked under the pane's generation) with each one's pixel size, and `request_rasters` asks for them as
  one batch (`SvgDraws::submit_batch`: one child for the frame's snippets, and for every other batched job waiting),
  marking each asked under that generation. A snippet is asked once per generation: a refusal is terminal for it
  until the source is installed anew (a later generation asks again) or its entry is replaced by a fresh reply, and a
  resize or a later frame asks nothing. An outcome counts only if the snippet was asked under the generation it
  carries (`settle_drawing`): a departed generation's `Stale` can arrive after the new one asked again, and changes
  nothing; it needs no reset, since the next paint asks again under the new generation. A batch the worker refuses at
  submit (a full queue) leaves its snippets asked, so they are not sent again each frame. The window keeps only the
  upload (`on_math_settled`). An inline math raster is sized to fit the preview pane's width and its reservation's
  height, never wider than the pane (`a_formula_wider_than_the_pane_rasterizes_at_the_pane_width`). Tests, in
  media_store_tests.rs (a stand-in draw): `a_math_snippet_is_submitted_once`,
  `a_refused_math_snippet_is_not_submitted_on_later_frames`, `the_snippets_of_a_frame_go_to_the_worker_together`,
  `a_departed_generations_outcome_does_not_undo_a_new_ask`,
  `a_stale_snippet_is_asked_again_under_the_current_generation`, `a_batch_refused_at_submit_is_not_asked_again`; the
  native fixture `a_page_of_math_is_drawn_by_the_worker_in_one_batch` draws 50 inline equations, all replied before
  its first frame, through the real child and asserts they went to one child (about 180 ms in a debug build on a
  loaded Linux host, against about 3 s with one child per snippet). Cost in use: the batch is of what is waiting when
  the worker pops, and each `math.render` reply arrives on its own, so a page costs one child per pop (a pop takes every
  snippet waiting then), not one per page; the 50-equation figure is the all-arrived case. Limit: a
  batch has the one wall (`SVG_WALL`) for all its snippets, so a page whose snippets together take longer is cut at
  it, the rest refused in its words; the old standalone math pane's draw is gone.
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
- `TerminalDrawer` privately owns the Terminal drawer's local shell (G2/G3) and its scroll motion: spawned the first time the drawer is
  shown, at 80x24 in the repo root; pumped before each draw (the report says when the pane looks wrong); its pty
  resized only when the drawer's cell size changes (at least 2x2). Typed and pasted input returns the view to the
  live tail (and ends what a scroll still owes); a wheel the shell's app tracks is forwarded as is. A person's wheel and
  PageUp/PageDown over the local ring ask its `ScrollMotion` (`State::scroll_ask`; a screen on the alternate screen is moved
  at once, as before), and `spend_scroll`, run after the pump whether or not the drawer is shown, moves the ring the rows the
  curve has reached; a move that comes up short (the ring's end, the live tail) stops it
  (`drawer::terminal::drawer::tests::a_page_scrolls_the_ring_a_row_at_a_time_and_typing_returns_at_once`, on a real shell,
  Unix only).
- `MonitorPane` privately owns the Monitor drawer (ADR 0020): its chart model, the raster, the drawer rect, the
  readings' dirty mark, the subscription this window holds and the declared hub it asks. The chart is the one SVG the
  window still draws in its own process (`quad_from_own_svg`): the window's process draws only SVG it composes, and
  the chart's text is clamped before it is composed, to the first 16 processes and the first 64 characters of each
  host, process and user name (`MAX_PROCS`, `MAX_NAME_CHARS`; `a_monitor_chart_holds_at_most_its_clamp`), so the
  sampler does not bound it. Limit: the number of hosts in a roster is not clamped. Each reply marks the
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
  The Chat drawer's records are the third selectable pane (B5q1): `ChatView` holds a `FlowedSelection` over one record's
  buffer (`reserved: None`) and that record, found again by its time and buffer key. A left press anywhere in the records
  area focuses the drawer; only a press on a body line (the record's buffer from its lead on, never its header line, the gutter or between records) starts
  a selection, and the drag is clamped to the record, its end never above the body's first position
  (`a_press_selects_in_the_record_under_it_and_a_drag_stays_in_it`). Any other press clears it; the chat's arm in
  `press_selections` follows the agent's and the preview's, and a drawing press clears it too
  (`preview_press_focuses_and_selects_and_agent_press_clears_it`,
  `a_press_the_drawing_takes_focuses_the_preview_and_starts_no_selection`). The frame ends the selection when its record's
  buffer changes: an edit, a delete, a record a snapshot dropped (a twin with the same words does not take its place), a
  new width or scale, a buffer the trim let go, and a row change (`a_change_to_the_selected_buffer_clears_the_selection`);
  the mark moving onto or off the record is no change to its buffer and keeps the selection (`the_mark_is_a_band_and_moving_it_keeps_the_selection`); a thread opening clears it before the channel's view is
  parked, so nothing selectable is parked and none returns at close (`a_selection_is_not_parked_with_the_channels_view_when_a_thread_opens`). The tint is the last of the chat's rectangles (`ChatView::backs`), over every fill and under the glyphs
  (`the_selection_lies_over_every_fill_and_is_the_last_rect`); it is not part of the `Solid::SelectionBg` paint of the preview's and the agent pane's
  (`paint_text_selections`). A chat selection is never shared with the
  agent. `chat.copy_selection` (`Primary+c`, `Primary+Shift+c`, the `Chat` scope: only in the Chat tenant, so the agent
  pane's, the Terminal's and the REPL's Ctrl+C are as before: `the_chat_copy_chords_resolve_in_the_chat_tenant_alone`)
  writes the selection's bytes through `input::write_clipboard_text`, else the marked message's shown text (its buffer
  after its lead, the header line if it has one), else the status line says `nothing to copy: select text or mark a message`
  (`the_copy_goes_to_the_clipboard_or_says_why_not`); the same buffer ranges copy what the preview copies, markup as shown
  (`chat_and_preview_copy_same_utf8_ranges`).
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
- `LayoutHelp` privately owns the shown drawer, maximize/wide flags, Help browsing, the first return origin and startup
  requests, including private nested storage. It returns owned effects, never takes State and lends only immutable
  observations. The drawer has four tenants (Repl, Terminal, Monitor, Chat) besides Help; Chat shows the held records (`drawer/chat/`) and
  its keys reach no REPL handler (`input/CLAUDE.md`). Ordinary toggles and a row switch abandon the origin and request Monitor sync, after focus and before binding
  feedback/redraw. Automatic REPL opening keeps setting/base-preset gates; demo opening ignores them; Terminal
  failure closes only selection. These paths preserve other layout/Help state and keep held subscriptions. A row switch
  replaces the drawer with the entered row's (`enter_row_drawer`, applied after the REPL and view restores): Help is the
  window's, so it closes, its hidden maximize returns with the focus it took (focus still in Help goes back to where Help
  was opened from) and its origin goes; `row_drawer` parks what Help covered. The
  effect requests Monitor sync, so a drawer left or entered as Monitor takes or drops the subscription as a toggle does,
  and moves focus to the tree only when the focus was in the drawer and the entered row's drawer is closed (an open one
  never takes focus). The Chat's watch follows `chat_shown`, so it follows the restored drawer. Pinned by
  `entering_a_row_shows_its_drawer_and_moves_only_drawer_focus_off_a_closed_one` and
  `a_switch_while_help_shows_closes_help_and_keeps_the_layout_it_hid` and
  `a_switch_while_help_shows_returns_the_focus_help_took_and_never_moves_it_into_the_entered_drawer`.
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
  discarded-input count, its scroll motion and composed `PanePresentation`. Its operations never take State or expose mutable
  clients/screens/storage. The narrow
  `PaneClient` port uses `FeAttachClient` in production and a scripted client in `agent_pane/owner_tests.rs`.
  State applies owned effects for transport sends, asynchronous retirement, logging, redraw, clipboard and persistence;
  it keeps host/dial configuration, pointer geometry/residue, create origins and multi-owner event ordering.
- The pane's wheel (only over its own ring: never when a program in it tracks the mouse, and a program on the alternate
  screen is moved at once) and PageUp/PageDown ask its `ScrollMotion` (`State::scroll_ask`), and `spend_scroll`, run
  after the pump in `pump_pane_attach_term`, moves the ring the rows the curve has reached and answers whether frames must
  keep coming. The client's own offset stays the one position; the edge (the ring's end, the live tail) is a move that came up
  short, read from the client's screen, and stops the motion. Typing and pasting (`reset_scroll`), a reshape's snap to live
  and a kept row's invalidation stop it; a switch finishes it first, so the parked ring stands where the person asked
  (`a_page_scrolls_the_ring_a_row_at_a_time_and_typing_returns_at_once`, `a_scroll_stops_at_the_tail_and_at_a_reshape_and_a_switch_finishes_it`).
  The State glue (the three call sites and `mark_dirty`) rests on reading.
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
  send as counted `RunEnded`, with its existing notice; unknown delivery is never retried automatically, and a key the row refused as from a client that is not the driver, after the capsule's one retake, is said on the status line as not sent (`agent_pane::attach_tests::keys_the_row_refused_are_said_not_sent`). The worker
  refuses any input before its first attach, so the pane never relies on a queue there. The notice that keys were
  discarded is the pane's count plus the client's own, drawn over the pane, and an attach, cold or warm, does not clear
  it. `DiscardNotice` owns the pane's count with the record of what a frame drew (the notice's row and the total shown),
  and its `dismiss` is the only clear: only a person's act dismisses it, and only a count a frame has drawn (a discard
  since then is unseen), so neither a row switch nor the daemon's kept-row reply does, and the notice, which names no
  row, goes on showing over the next row. The client's own count lives with its client: it leaves with a retired one and
  comes back with a parked one. The acts are a key held or sent, Esc, Backspace or the Enter that sends, or a click
  on the notice's row. A key that holds or sends nothing (a modifier alone) leaves it, a discarded key adds to it, and a
  dismissal clears both counts (`a_discard_notice_outlives_the_attach_until_a_key_or_a_click`,
  `typed_keys_past_the_hold_add_to_the_notice_and_a_key_that_does_nothing_leaves_it`,
  `a_discard_after_the_last_frame_is_not_dismissed_unseen`,
  `the_enter_that_sends_held_keys_dismisses_the_drawn_notice`, `the_clients_own_discard_count_is_dismissed_with_the_panes`,
  `a_row_switch_does_not_dismiss_the_notice`, `a_kept_row_reply_does_not_dismiss_the_notice`,
  `a_click_on_a_held_line_leaves_the_notice_and_a_click_on_its_row_dismisses_it`; a full hold says so, `an_enter_the_full_hold_refuses_says_so`).
  A client that leaves the pane (dropped by a switch, evicted from the warm pool, retired dead, retired with its row, or
  by the kept-row invalidation; a parked client keeps its queue and sends it) is retired through the capsule's `retire`,
  and the inputs its worker never took, and the held keys of a row pruned away, add to the count
  (`a_client_retired_with_input_waiting_is_counted_as_discarded`, `held_keys_dropped_by_pruning_are_counted`). A wheel while nonlive only
  scrolls locally or reports Pending and never holds mouse bytes. Behavior tests in `agent_pane::owner_tests`:
  `keys_typed_while_a_row_connects_are_held_and_sent_only_on_an_armed_enter`,
  `held_keys_stay_with_their_row_across_a_switch`, `held_keys_are_not_sent_into_a_run_the_screen_did_not_show`,
  `held_keys_past_the_bound_are_refused_and_counted`, `esc_drops_and_backspace_removes_the_last_held_key`,
  `a_repeated_enter_never_sends_held_keys`, `a_wheel_while_connecting_holds_nothing`,
  `held_keys_show_safe_text_and_the_tail_that_fits`, `a_redial_or_new_screen_run_requires_another_presented_frame`,
  `ordinary_input_and_wheel_name_the_shown_run`, `pruning_drops_only_the_removed_hosts_held_rows` and
  `kept_row_invalidation_preserves_the_existing_scope`.
  Held pixels never receive input. Copy uses client-or-blank and clears selection only after clipboard success.
  A copy (`agent.copy`, `Primary+Shift+c`) joins a row the terminal wrapped onto the next (no newline, trailing spaces
  kept; a wide character that found one column left wraps and flags its row in the vt100 fork, its empty cell no text)
  and ends every other row with a newline, a program's own line breaks kept, Claude Code's word wrap of its replies
  included (continuation rows indented two columns, the break's space dropped), since the window cannot tell them from
  a real newline (`copy_joins_a_wrapped_row_and_breaks_where_the_program_did`,
  `a_row_that_exactly_fills_the_width_and_ends_in_a_newline_is_not_joined`, `a_wrapped_row_keeps_the_spaces_at_its_wrap_point`,
  `a_wide_character_that_did_not_fit_the_last_column_copies_joined`,
  `the_default_copy_keeps_every_break_the_program_made`: a rule, a table border, a path or a URL starting the next row is
  not a wrap). `agent.copy_line` (Copy as one line, `Primary+Shift+l`, in the Agent pane's help beside Copy selection) is the
  explicit gesture for a command or paragraph the person knows is one line: the same `copy_text` with `one_line`, where
  terminal wraps join as above and every other break is one space with the next row's indent dropped; a blank row is
  skipped (a gap between paragraphs is one space too, no trailing space); where the row above reaches the last column
  the program may have cut a word wider than its row (Claude Code's wrap-ansi, `trim: false, hard: true`, read from the
  code's options), and the selection's own continuation indent (the smallest first-text column of the rows after the
  first) decides: a next row starting there continues the word (no space), one starting further in is the row wrap-ansi
  starts with a space after an exactly full one (one space) (`cut_word`, `continuation_indent`;
  `copy_as_one_line_joins_a_command_the_program_wrapped`, `copy_as_one_line_keeps_a_word_cut_at_the_last_column_whole`,
  `copy_as_one_line_adds_a_space_after_a_word_that_ended_exactly_at_the_edge`,
  `copy_as_one_line_keeps_the_space_before_a_long_word_after_a_row_that_ends_at_the_last_column`,
  `copy_as_one_line_skips_blank_rows_and_text_the_selection_does_not_reach`). Only the agent pane copies from a terminal
  screen. Limits: both copies rest on the terminal's per-row wrap flag, and a width change clears the flags until the
  program redraws; the one-line copy joins every row in the selection, so a list, a table or a path list selected into
  it becomes one line (the person chose it); the cut-word join assumes the program wraps at the pane's last column (the
  pane's width less its indent), since the selection's widest row cannot tell that width from its longest ordinary
  row, which would then read as a cut: a program wrapping narrower leaves a space inside a cut word
  (`copy_as_one_line_needs_the_program_to_wrap_at_the_panes_width`); with fewer than two continuation rows there is no
  indent to compare, and a row that reaches the last column joins with no space only when its last word and the next
  row's first would not fit one row together; a real newline after a row that fills the pane exactly, followed by a row
  at the indent, joins as a cut word; and a wide character's second cell adds no space
  (`a_wide_character_copies_without_a_space_for_its_second_cell`).
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
- An SVG's `<image>` is drawn only from embedded `data:` data (`embedded_images_only`, preview/image/svg.rs); a path or
  URL reads nothing. Limit: an SVG that links a raster by path now draws without it and says nothing. Tests
  `an_image_href_naming_a_local_file_draws_nothing`,
  `an_href_naming_a_fifo_draws_nothing_through_every_element_and_spelling`, `an_embedded_png_image_still_draws`.
- The drawing child, `sot --draw-svg <heap bytes> <cpu seconds> --version` (preview/image/svg_child.rs, whose numbers
  are preview/image/svg_wire.rs): the window's own binary, so that SVG whose bytes the window received can crash, exhaust
  or spin only a process the window can lose. Its callers are the markdown figure and the SVG file preview, through the worker (the bullets below). The first statement of `window_entry` (main.rs, which both `main`s call) dispatches on the first argument
  and never reads the last, so a binary without the mode prints its version and exits 0. The child bounds itself before
  it reads a byte (`bound_self`, in this order, platform's guarantee 18): no descriptor above stderr (Unix), stdout taken
  for the replies (`take_stdout`), crashes as exits, `end_with_parent` (Linux), `limit_cpu` (Unix), the heap armed (`HEAP`, main.rs's `#[global_allocator]`, a
  `HeapCap` that costs a load and a compare per allocation while disarmed, which is every other mode). Request on
  stdin: `SVG1`, a u32 job count, then per job an ask, a u64 length and that many bytes of SVG (the ask is `S` and `w` and
  `h` as u32, draw onto exactly that, or `F` and a pane's `w` and `h` and a texture limit `cap` as u32, draw to fill
  the pane, the size being the child's to work out from the file's own: aspect kept, no side past `cap`, at most
  `MAX_RASTER_PIXELS`, 16 Mpx, in all); the SVG bytes' sum is at most `PREVIEW_BINARY_CAP` (the headers are not counted) and each job's bytes are
  allocated at their declared length, so the cap sees an input before it is read. Reply on stdout: `SVG1`, then per job
  in order `R`, `w` and `h` as u32 and `4 * w * h` premultiplied RGBA bytes (a zero side is drawn as one), or `E`, a u16
  length and at most 1 KiB of the error's first line, cut at a character boundary; an SVG that does not parse is that
  job's `E` and the next job still draws. Each reply is written when whole, so a later job that ends the child costs
  only itself. The error text holds the file's own bytes (a parser message names an unknown entity) and is untrusted
  where it is shown. The replies are written only to the private copy of stdout that `take_stdout` returned, and
  descriptor 1 leads to the null device, so a library's print (usvg prints a fixed warning line for some color-font
  text, usvg-0.44.0 text/colr.rs:185 and :285) is lost and cannot arrive between two replies; stderr is the window's to
  close. Only a descriptor or handle copied before `take_stdout` could still reach the replies, and `bound_self` calls it second, before anything else runs (platform's guarantee 18). Jobs draw on one thread of 32 MiB
  (`SVG_STACK`, the same on every system; Windows gives a main thread 1 MiB); a panic there ends the child with 73 at
  once. The child exits 0, or: 71 at its heap cap, 72 at its CPU bound (Unix), 73 in a crash (on Unix an abort, Rust's
  stack-overflow report or a panic on the draw thread; on Windows only a panic on the draw thread), 74 when its
  arguments, its request or its bounds cannot be had (no reply is written), when the draw thread cannot start (none
  either), or when a reply cannot be written (the replies before it are whole). On Windows an abort or a stack overflow
  ends with the OS's own crash status, and there is no CPU bound: the parent's wall clock is the only time bound.
  Limits: a crash by `SIGSEGV` outside a stack overflow, `SIGBUS` or `SIGILL` ends by signal, and with a core limit
  above zero and a plain core pattern can leave a core holding the file's bytes in the window's working folder; the heap cap counts requested
  bytes, so memory the file can reach without the allocator (fonts and the stack are fixed mappings) is not counted;
  nothing is enforced on what the child may open beyond what the code does (an `<image>` reads nothing, the child
  starts no process).
  Tests (`rust/frontend/tests/svg_child.rs`, each spawning the built binary under a cleared environment, stderr
  null, a scratch folder as working folder and `HOME`, and on Unix a core limit): `a_plain_svg_comes_back_as_its_pixels`,
  `every_job_of_a_request_is_drawn_in_order_and_an_error_does_not_end_the_child`,
  `replies_before_a_crash_arrive_whole`, `an_error_text_is_cut_at_a_kilobyte`,
  `a_request_that_is_no_request_is_refused_with_its_code`, `an_input_past_the_heap_ends_with_its_code`,
  `a_draw_past_its_heap_ends_with_its_code`, `an_entity_expansion_past_its_heap_ends_with_its_code`,
  `a_reference_cycle_across_two_kinds_ends_the_child_and_not_the_caller`, `a_child_past_its_cpu_bound_ends_itself`
  (Unix), `a_library_print_never_reaches_the_reply_stream` (debug builds: a hook prints between replies, as usvg does), `a_panic_on_the_draw_thread_is_the_crash_code` (debug builds, where a file that panics in resvg's debug
  assertions exists), and in svg_child_tests.rs `an_error_is_its_first_line` and
  `an_error_is_cut_at_its_limit_and_never_inside_a_character`. Seen once on a Windows 11 computer (2026-11-12, this
  test binary on a debug build, each crash test alone and then the whole binary): the reference cycle's stack overflow
  ended the child with the OS's crash status in 0.15 to 1.3 s, and the three crash tests showed no error window
  (screenshots and the window list), started no error-reporting process (a 100 ms watch), and left no new crash dump,
  error report or Application-log event; that computer's error reporting was at its defaults (platform's guarantee 18).
- The runner (preview/image/svg_run.rs, a leaf on std and platform that the test target includes; its caller is the
  worker, through `draw_in_own_child`): `draw_in_child(command, jobs, bounds, stop)` starts the drawing child and
  returns one `Outcome` per job, in order: `Drawn` (w, h, RGBA); `Ended` with the words of a child that hit a bound or
  crashed (`took past`, `needed more than`, `the drawing ended`); `Refused` with any other words a person reads (the
  drawing's own first line of error, a bad reply, `not a drawing program`, `cannot start the drawing`); or `Stale`, the
  jobs a `Stop` cut short. A `Stop` (the worker's off switch for one draw) wakes the runner's wait at once, and a stop
  that comes before the wait begins is met when it does; the child is killed and reaped, and every job it had not
  answered is `Stale`.
  - Start (`start`): a cleared environment that holds only what the font code reads (`child_env`: `HOME`,
    `FONTCONFIG_FILE` and the `XDG_` config, data and cache folders on Unix; `SYSTEMROOT` and `USERPROFILE` on Windows,
    in any case), stderr null, stdin and stdout piped, around `fork_guard` (macOS's birth lock) and with no `pre_exec`,
    so std keeps `posix_spawn` and the start's cost does not grow with the window's memory. On Windows the child gets
    `CREATE_NO_WINDOW` (`sot.exe` is a console program) and is assigned to a kill-on-close `AnonymousJob` before `start`
    returns, so before any input byte; an assignment that fails is an error. `Started` kills and reaps its child when dropped,
    so no return or unwind (a panic, a thread the OS refuses) leaves one running. The program is the
    window's own binary (`own_program`: `/proc/self/exe` on Linux, so a computer with no `/proc` cannot start it and
    every draw is `cannot start the drawing`; elsewhere `current_exe`), its arguments `Bounds::args`. `SVG_BOUNDS`: a
    wall of 10 s (about twice the dearest lawful drawing measured, 4.7 s), a heap of 2 GiB (twice the allocator's
    high-water mark of the dearest of three drawings measured at 16 Mpx, 0.58 GB, rounded up to a power of two) and a CPU
    bound of 11 s, the bound of a child whose window died mid-draw on macOS.
  - Finish (`finish`): the request is written on one thread and the replies read on another (a binary without the mode
    never reads its input, and must still be read from), while the caller waits on a channel to the wall. Each reply is
    checked as it arrives: the first four bytes are `SVG1` (else `not a drawing program`, at once, whatever the input's
    size), a picture's size is what its ask allows (`Ask::accepts`: a stretch's own size, a zero side counting as one as the child draws it; a fit's within `cap` and the pixel budget) before its body is read,
    and the body is read as it comes, so a size the child only claims allocates nothing; an error text is at most 1 KiB
    of UTF-8; after the last reply the stream must end. A reply that breaks its form refuses every job with `the drawing's
    reply was not what was asked`. The child is always killed and reaped when `finish` returns, the reap being the one
    unbounded wait (an uninterruptible sleep), as in containment's guarantee 4. Jobs a child did not answer get the words
    of its end: `took past 10 s` at the wall, and also for a child that closed its stdout and lived, which the runner then
    kills (told by its status: SIGKILL, or code 1 on Windows, where the child's own codes are 71 to 74); `needed more than
    2048 MiB` at exit 71 (the exit status wins over a pipe error, so a child that ends at its heap while its input is
    being written says so), else `the drawing ended (<exit status>)`. Replies already read stay: a hostile job spoils only
    the jobs after it in its request. The two threads are not joined: each ends when its pipe reports the child's end,
    which is at once unless another process holds a pipe end (a start elsewhere in the window that inherits it); then
    the reader waits for that process, `finish` still returns at the wall, and the thread lingers until that process
    lets go.
  - Limits: the child starts no process, and the start's `#[allow(clippy::disallowed_methods)]` says why it stands
    (containment's guarantee 9); on Windows the child runs for a moment outside its job, before it is assigned and given
    any input; an error text a child sends holds the file's own bytes and is untrusted where shown.
  - Tests (`rust/frontend/tests/svg_child.rs`; stand-ins are `/bin/sh` with `/usr/bin/printf` and `/bin/sleep`, Unix):
    `the_runner_returns_each_job_as_its_pixels_or_its_error`, `a_program_that_cannot_be_started_is_refused_in_words`,
    `a_draw_past_its_wall_bound_is_killed_and_refused` (the pid is gone after), `a_heap_end_during_the_input_write_says_heap`,
    `the_childs_environment_is_the_font_list`, `a_binary_without_the_mode_is_refused_at_once` (a 1 MiB input),
    `bytes_after_the_last_reply_are_refused`, `a_reply_past_its_ask_is_refused`, `a_reply_that_breaks_its_form_is_refused`
    (an error text past 1 KiB, one not UTF-8, an unknown tag), `a_child_the_runner_has_to_kill_is_refused_in_the_walls_words`,
    `a_dropped_start_ends_its_child`, `the_runners_own_kill_is_known_by_its_status`, `a_child_reads_no_descriptor_of_the_window`
    (Linux: the real child, drawing, holds no descriptor the window left open, has the null device for stderr and only the
    font variables, plus the few its shell adds, in its environment) and
    `a_started_child_is_in_its_job_before_it_is_given_any_input` (Windows) and
    `a_stopped_draw_is_killed_at_once_and_stale` (a stop during the wait and one before it) and
    `a_child_that_ends_early_refuses_the_rest_of_its_batch_in_its_words`.
- The preview source's generation (`InstalledPreviewSource::generation`, preview/source.rs) says which source the pane
  shows, for the drawing worker: a process-wide counter, so a number is never reused. An install, a snapshot's restore
  (`renewed`) and an emptied source (`Default`) each take a fresh one; a repaint of the same source, a reflow or a
  text-scale change, keeps it. Tests, in source.rs: `a_new_source_is_a_new_generation_and_the_same_source_is_not`,
  `a_restored_or_emptied_source_never_reuses_a_generation`.
- The drawing worker (preview/image/svg_draws.rs): `SvgDraws`, owned by `State`, is the one owner of the window's SVG
  draws; the markdown figures, the SVG file preview and the math snippets are its submitters, so no SVG whose bytes the
  window received is drawn in its own process. One worker thread, started at the first submit, draws one batch at a
  time: a job from `submit` is a batch of one, and the jobs from `submit_batch` (math) join every other waiting one of
  their kind in one draw, so one child (a child's start costs about 74 ms on Windows). A batch is queued whole or
  refused whole (`too many pictures waiting`). The window's
  thread and the worker share one lock over the queue, the bytes waiting and the running draw's `Stop`, never held
  across a draw or the wake, so the window's thread never waits on the worker.
  Rules:
  - A job carries the generation of the preview pane's source (above). `frame_upkeep` tells the worker the current
    generation after `drain_events`, and `submit` first advances to its own job's (a submitter has just read the pane's,
    so a newer one than the frame's last is current), so a job is never stale for being early. A job of an older
    generation is `Stale` at once.
  - The queue holds the current generation alone: `advance` hands every waiting job of an older one back as `Stale` at
    once and stops the running draw (`Stop`), so a worker slow to return holds none of the old queue's memory, and a
    hostile file's pictures neither wait nor count against the next file's bound. A job is never given to the draw
    after its generation went stale. An outcome of a generation that is no longer current when `drain` reads it is
    `Stale`, whatever its draw said (`drain` holds the pane's generation). An outcome belongs to the generation it
    carries: the owner submits a stale key again under the new generation, and ignores an outcome that is not its
    key's pending generation, since a departed generation's `Stale` can arrive after the new generation submitted the
    same key.
  - One draw at a time (the worker's own thread draws); the queue holds at most `QUEUED_BYTES` (`PREVIEW_BINARY_CAP`)
    of the current generation's input from its submit to its start, and a submit past it is `too many pictures
    waiting`.
  - After `SVG_STRIKES` (2) draws of one generation end `Ended` (a bound or a crash; the drawing's own parse error
    is not a strike), its later jobs are `an earlier picture in this file was refused`, without a child; a new
    generation starts clean. A strike is one child's end, not one job's: a batch whose child ended early (the
    jobs it had not answered come back `Ended` in its end's words) is one strike.
  - Outcomes go on a channel that `State` drains at the top of each frame (`SvgDraws::drain`, read in `frame_upkeep`
    through `svg_preview::settle_frame`, which hands each outcome to its owner by key: the preview, a math snippet or a figure); each the worker sends is followed by one
    `MainWake::wake` (the stale ones `advance` and
    `submit` hand back are read by the window's own thread and wake nothing). A draw that panics gives no outcomes, and each
    of its jobs is refused in `cannot start the drawing` words; a worker thread the OS refuses makes `submit` itself
    answer `cannot start the drawing`, with nothing queued and no outcome to follow. Dropping
    `SvgDraws` stops the running draw and ends the worker without joining it. A child left running when the window
    exits ends with the worker thread that started it on Linux (`end_with_parent`), at its CPU bound on macOS and with
    the job's last handle on Windows. Limit: the reap after a kill is the runner's one unbounded wait, and while the
    worker waits in it no later job is drawn.
  - Tests (preview/image/svg_draws_tests.rs, a stand-in draw held until released or stopped, no child):
    `a_new_generation_starts_after_at_most_the_stale_run_is_killed`, `a_stale_job_is_skipped_before_it_starts`,
    `one_draw_runs_at_a_time`, `a_submit_past_the_queued_bytes_is_refused`,
    `a_stale_generations_queue_does_not_refuse_the_current_one`, `after_two_strikes_a_generation_draws_no_more`,
    `only_a_bounds_end_is_a_strike`, `an_outcome_reaches_the_drain_and_wakes_once`,
    `the_window_never_waits_behind_a_slow_wake`,
    `a_job_of_a_newer_generation_than_the_frames_is_drawn_and_an_older_one_is_stale`,
    `the_first_submit_before_any_advance_is_drawn`, `a_draw_that_outlives_its_generation_is_stale_whatever_it_says`,
    `dropping_the_draws_stops_the_draw_and_ends_the_worker`,
    `the_waiting_jobs_of_a_generation_that_batch_are_drawn_by_one_draw`,
    `a_batch_that_ends_early_refuses_the_rest_in_its_words_and_strikes_once`, `a_batch_past_the_queued_bytes_is_refused_whole`
    and `a_draw_that_panics_refuses_its_jobs_and_the_worker_goes_on`.
- Harness instances (`ephemeral`: `--ephemeral`, any `--capture`) write no shared file and arm no watcher:
  `persist_resume_state` (persist/mod.rs), `maybe_write_fe_state` and the command watcher (control/file_channel.rs),
  `resumed` (app/handler.rs); a relaunch command is refused for them (control/dispatch.rs).
- A presentation receipt requires the current live attached client's checkpoint, a nonempty painted pane and a known request origin, and is emitted once only after frame presentation.
- Showing a result never steals the view: `route_fe_command` (control/command.rs) honours force-show only for a
  command addressed to this frontend, and a broadcast `relaunch` is refused.
- A path an agent sends (a preview or a reveal from the daemon's relay or the command file, a `sot_ui` envelope) is
  taken in the spelling the Files tree lists and the watcher reports (`nav::files::node_path`): empty and `.` names
  dropped, each `..` folded into the name before it (text only, no symlink followed: docs/defects.md: PATH-FOLD-LEXICAL),
  and a rooted path or one that leaves the workspace is no command (the command file's and an envelope's is dropped with
  a log line; the relay's is named on the status line). The daemon
  refuses a node id holding `..`, and the fired target, the cursor and `preview.changed` compare ids as text, so a
  path in another spelling would be refused or never refreshed (docs/defects.md: PREVIEW-PDF-STALE). Test:
  `control::command::tests::an_agents_path_is_taken_in_the_trees_spelling`.
- A preview the daemon refuses leaves the pane on what it showed; while the refused node is the fired target the pane's
  title ends `REFUSED, view not updated` (`preview_requests.refuse`). The mark parks in the row's snapshot with the older
  bytes and returns with them; a reply that installs, a new request or a first visit clears it. A reply for a newer
  generation than a badged result's attempt satisfies the attempt (`preview_installed`). Tests:
  `preview::requests::tests::a_refused_preview_of_the_fired_node_is_marked_until_something_replaces_it`, the native
  fixture `passing_a_result_row_keeps_its_badge`, `session::result_ledger::tests::a_newer_reply_for_the_attempts_node_satisfies_it`.
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
  `chat.history` read; a pending watch is sent again by `tick`.
  Watches go out at 0, 250 ms, 1 s, then every 5 s (`FIRST_PACE`, `SECOND_PACE`, `PACE`).
  The same schedule serves an `unavailable` answer while the service starts (one extra watch against the old 0, 1 s, 6 s)
  and a snapshot read the daemon refused (a retry at 250 ms and at 1 s, not at 1 s only); the gap rules, `GAP_PACE` and the
  20 s window are as before.
  While a watch waits on one of the two short paces, `ChatOwner::next_ask` names the instant of its next ask (`None` when none
  waits, the daemon is unsupported, a read is in flight or the state is a gap).
  The idle loop's `about_to_wait` joins that instant to the other idle deadlines (a help peek's fade, the quit hint, the reply
  bar's arm moment) through `earliest`, so the window sleeps to each ask, draws no frame in between, and the chat delays none
  of them; an ask whose instant passed before the loop went to sleep is a redraw at once (`has_passed`).
  Tests: `a_pending_watch_is_asked_again_soon_and_the_window_is_woken`, `the_drawer_names_the_instant_of_a_first_watchs_next_ask`,
  `no_frame_is_wanted_for_an_ask_that_will_not_go`, `no_ask_is_named_while_a_read_is_in_flight`,
  `handler_idle_tests::the_chats_next_ask_joins_the_idle_deadlines`, `handler_idle_tests::an_ask_that_has_passed_is_a_redraw_now`.
  The calls in `about_to_wait` are read, not run, as for the other three deadlines.
  A recovery keeps its start and pace through a loss other than the link (a link change starts over), and a `complete`
  snapshot ends it.
  A refused watch is a gap at once and forgets the registration (the daemon took it back); for a watch the daemon admits or never answers, `Gap(reason)` is shown only after the daemon's slowest snapshot: 20 s from the first watch it admits (any answer but `unavailable`; with none, from the recovery's start, so a watch never answered still ends in a gap), the last watch getting its full pace to be answered
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
  daemon's model does (it sends no edit or delete for a thread reply sent to the channel). The owner keeps up to 256 records above
  its floor (`KEPT`), dropping the oldest (`the_owner_keeps_at_most_the_daemons_newest_records`), but never the record the reader is
  anchored on or a newer one (the view's anchor, handed to the owner each frame: `the_readers_anchor_reaches_the_owner_each_frame`),
  so a reader on the oldest meets no move while the store passes `KEPT`, up to `CEILING` (`KEPT` and one page, 276; past it the
  oldest go anyway: `a_parked_reader_holds_the_store_to_the_ceiling`). One pass (`bound`) trims after every change to the records,
  a snapshot's merge included (`every_change_is_bounded_and_the_overlap_is_at_or_before`). `reserve(n)` (B5p) tells the owner how many of the `KEPT` records an open thread holds (at most 156, so the channel keeps at least its newest 100): the bound, a page's room and the asking for a page all use the channel's share, `KEPT - n`, and `reserve(0)` gives the room back (`a_reserve_takes_room_from_the_channels_oldest_and_gives_it_back`); `thread_outs` calls it with the thread's size and `close_thread` with 0. A snapshot with a `next` that reaches back to the oldest held record replaces the records from
  its own oldest up and keeps the older ones, tombstones included, lowering the floor if it reaches below it; one that does not reach clears them and its oldest is the floor;
  one without `next` replaces all (`older_pages_preserve_the_anchor_within_the_bound`). Older pages: when the view reports the
  oldest held record in the body (`ChatView::at_oldest`, handed over each frame), the floor is set and the store has room for a
  page (fewer than `KEPT` held; a page joins only what fits), the owner asks `chat.history {upper: before(floor), limit: 20}` in its own slot (`paging`, apart
  from the snapshot's `reading`, and in the drawer's reads-in-flight set, which orphans it when the watch is replaced or a
  snapshot clears the older records); the reply is taken only while it equals that slot and its upper bound is still `before(floor)`; as many of its newest top-level
  records as fit in `KEPT` join below the floor, which falls to the oldest joined, or to the page's oldest while it had a `next`
  (`older_pages_join_below_the_floor_without_a_hole`, `a_page_is_taken_only_below_the_floor_it_was_asked_for_and_within_the_bound`).
  Paging is a condition: a refused page is asked again after `PACE` (30 s for `limited`; never for `stale_row`, `stale_mapping`
  or `unmapped`, until a link or row change: `a_page_refused_for_good_is_asked_again_only_after_a_link_change`), a page cut at
  Slack's plan ends it, and an empty page or snapshot that says more history exists keeps the floor and stops the paging until a
  replacing snapshot (`an_empty_page_with_more_history_stops_the_paging_and_says_so`; the daemon's dropped events can make one,
  and the window cannot move past it without a cursor). The title says which, or that the full store (`KEPT` held) hides older
  messages (`a_full_store_a_refused_page_and_a_hidden_history_are_told`). A
  `floor` (set by a snapshot that is not the whole channel, from its oldest reply when it holds no top-level record: `a_snapshot_of_only_thread_replies_still_sets_the_floor`; raised by the trim) bounds what is added: a `put` of a record not held
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
  (`changed_events_put_and_delete_the_watched_channels_records`): the owner holds each record with its deleted flag as one pair (`records()` returns the pairs), keeping only the deleted record's identity, author, time and origin, and the view passes the flag to the projection, which shows the header and `(message deleted)` alone (`a_deleted_message_is_its_header_and_the_notice_alone`, `a_deleted_record_shows_as_deleted_through_the_drawer`, `an_anchor_on_a_deleted_message_passes_on_when_a_snapshot_drops_it`). A deleted record counts against `KEPT`, a snapshot holds no deleted record from its oldest up, though the deleted ones below an overlapping snapshot stay, a put of the same time brings the message back (`a_put_after_a_delete_brings_the_message_back`), and a delete of a record not held adds nothing (`a_delete_leaves_a_tombstone_in_place`).
- `stop` keeps the watch it ends when it holds records (its reads are given up all the same), trimmed to `KEPT` with no
  anchor (`a_parked_watch_is_trimmed_to_the_kept_records`): the owner keeps the last two (`PARKED`), newest first, and a
  channel that held nothing parks nothing (`a_watch_that_held_nothing_parks_nothing`). A `watch` of the same host and
  channel, for whichever row, takes up the parked one's records, floor and reach, and its word that Slack's plan hides older messages (a link change keeps that word too, `hidden_history_survives_a_switch_back`, `hidden_history_survives_a_links_return`), and begins as a recovery, as after a link's return
  (`Needed`), so a switch back draws what was held at the hub's answer while the channel is read again, and another channel
  never shows them (`row_channel_tests::a_switch_back_draws_the_channels_records_while_it_is_read_again`). Whether the
  snapshot reaches back to the held records is judged against the newest record held when the state was last whole
  (`Watch::reach`, none if it never was), never against what a recovery's own `chat.changed` puts have since raised the newest
  held to: those are not history, and no hole or missing floor results.
  `reach` is set when a whole state ends (a link change, a loss from `Complete`, a park), kept through any recovery that begins
  again (a link flap, a park and take-up) and taken by the snapshot.
  Tests: `a_put_between_the_take_up_and_the_snapshot_leaves_no_hole`, `a_put_between_a_links_return_and_the_snapshot_leaves_no_hole`,
  `two_link_flaps_with_a_put_between_leave_no_hole`, `two_switches_away_and_back_with_a_put_between_leave_no_hole`,
  `a_first_snapshot_with_a_put_during_its_read_keeps_a_floor`, `a_put_in_the_first_read_then_away_and_back_keeps_a_floor`,
  `puts_around_a_loss_in_the_first_recovery_leave_no_break`, `the_same_puts_away_and_back_leave_no_break`,
  `a_snapshot_takes_the_reach_it_was_judged_against`, `a_snapshot_reaching_back_to_a_parked_whole_state_keeps_what_it_held`,
  and `a_walk_over_three_channels_leaves_no_hole_and_a_floor_wherever_one_is_owed` (12,000 fixed seeds of 40 steps over three
  channels: row switches, link losses and returns, change events, lost messages, snapshots as of the read's send or its
  reply, refused reads and older pages, some of them `limited`; after every step the held records are the watched channel's, within the bounds, and
  in `Complete` one run ending at the channel's newest with a floor, or the word that Slack's plan hides older messages, wherever older history is owed).
  Limit: held records older than the snapshot's oldest stay, as the drawer keeps any older than its newest 100, so an edit or
  delete of one, and a parent's reply count and latest reply, made while its channel was parked do not reach the window for as
  long as the channel stays among the last two; the manual says so.
  The parked records stay in memory when the drawer is hidden.
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
  maps is not asked for again. An answer for a row no longer wanted is nothing. A row that changed or went ends the old watch at once, with the owner's `stop` (which parks it) and one `chat.unwatch`. A link coming
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

Limits: for admitted or never-answered watches, a link that drops more often than every 26 s never shows a gap (a refused watch is a gap at once); records of the window's page below the daemon's own set (deletions between the daemon's read and the window's) are held and never updated; the `State` routing in `ui/events.rs` (`on_chat`, `on_chat_bus_lost`, `on_chat_connection`, `tick_chat`) and `with_chat` are run by the native fixture `a_chat_of_200_messages_scrolls_posts_and_recovers` (`drawer/chat/native_fixture_tests.rs`, in `run_state_fixtures`, so only under a display: it lists a row, shows Chat, answers each request the window sends by kind and value through the window's own event queue, pages to 200 records, scrolls (the wheel through `mouse_wheel`, the window's own adapter), opens and closes a thread, takes 256 records of 8 KiB and a canvas resize, types, pastes, posts once, and sends a `Disconnected`, a `Connected` and a `bus.lost`; it asserts that every frame presents, the requests the connection carried by kind (the link's return brings one watch and one read, `bus.lost` one watch), that the REPL's draft and scroll are unchanged (this guards the events and the frames and the wheel, not the key routing) and that each frame's prepared areas hold the anchored record, and prints `chat-witness frames=… max_ms=… p95_ms=… resize_ms=…`, asserting no time; it is red with `tick_chat`'s wanted row removed and with `Pane::of_drawer(Chat)` mapped to the REPL); its keys are resolved by the real bindings under the real help context and routed by `witness_key`, a copy of the Chat branch of `drawer_pane_key`, of `chat_key` and of `scroll_line_chords`' Alt+Up branch, which skips the layers that run before them in `route_key_layers`: a winit `KeyEvent` cannot be built outside the event loop, so those three functions are not run and breaking any of them leaves the fixture green (a key press carrying its data, as the reply bar's key does, would delete the copy; not built), and neither is the window's own `Resized` event (the resize is the canvas's, the X window is not changed); `tick_chat`'s own
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
  the text goes through wire's `text_segments`, Slack's tokens are shown as Slack shows them by `mrkdwn::resolve` (`project_tests::links_mentions_and_specials_show_as_slack_shows_them`): a link its label, else its URL (a `Link` span in Sky Blue, not clickable, its URL never shown beside a label), `<@U..>` or `<@W..>` as `@` and the name the record's `names` holds for its id (the daemon looked it up; `mrkdwn::styled` gives each person's or channel's token its cleaned name, which `resolve` shows, so it is applied only where a token is resolved and a token inside inline code or a code block stays as typed; the name loses the characters a header's name loses and is trimmed, and a name with nothing visible left is no name), else the token's label, else the id (`project_tests::a_mention_shows_its_name_or_its_label_or_its_id`; the window never asks for a name), `<#C..>` or `<#G..>` as `#` and the name the record's `names` holds for its id (the daemon looks up a channel token with no label by `conversations.info` and adds the name only for a channel Slack says is not private, so a private channel shows its label or id; applied as a person's name is, where the token is resolved and never inside code), else the label, else the id (`project_tests::a_channel_mention_shows_its_name`), `<!here>`, `<!channel>` and `<!everyone>` by name and a group's label (`Mention` spans), and a date its fallback as body text; any other token, or one with nothing to show (a group or date without a label), is a `Markup` span shown decoded with its brackets, as before. Slack's inline styles (`*bold*`, `_italic_`, `~strike~`, `` `code` ``) are read by `mrkdwn.rs`, pure, over those segments (each token one non-space atom) and become the `style` bits of body spans, never a kind (so no markup makes a header: `project_tests::no_body_markup_makes_a_header`): a marker opens at the text's start or after whitespace or ASCII punctuation (never a non-ASCII one) and before a non-space that is not the same marker (so `**x**` shows `x` bold between two literal asterisks), closes at the nearest same marker on its line that follows a non-space and precedes the end, whitespace or punctuation, leaves the shown text, nests (`*_x_*`), and anywhere else (inside a word, unclosed, closed on another line, a run of backticks at an opener that no closing fence follows) is literal; code takes no inner style and shows a token as its decoded text (`mrkdwn_tests::slacks_inline_styles_follow_its_boundary_rules`). Blocks are found first, over the same atoms (`mrkdwn_tests::code_blocks_and_quotes_are_found_as_slack_writes_them`): a fence (three backticks) opens a code block and the next closes it, across lines, one line end after the opening and one before the closing dropped and the fences not shown; the block starts and ends its own line, its text is monospace at code's size in the body's color with no style, token or quote read (a token shows as its decoded text), and a fence with no closing one is as typed (the first failed closer scan ends all fence scanning, so a line of fences is one pass). A line outside a block starting with `>` (Slack's `&gt;`) and an optional space is a quote line shown without them, and `>>>` quotes its line and every later one outside the blocks, `>` then as typed; styles and tokens work inside a quote. Each block's pieces carry style bits beside the inline ones (`QUOTE`, `BLOCK`, `ALT`, `FILL`), which `styled_attrs` turns into glyph flags: `ALT` is on every other block, so two blocks that meet are two panels (there is no block id). An empty line in a block or a quote is shaped as one space that carries `FILL`, so it has a glyph and its flag; the space is in no copy (`FlowedText::shown`, which `FlowedSelection::text` calls for each line, is the view's `shown_line`, and `marked_text` uses it too: a line that is that one flagged space copies as empty, by the flag, so a space the poster typed on a line is kept). The panel and the bar find their lines by buffer line (`line_blocks`: the first span's flag of each layout line's buffer line, so the empty line holds its place with no glyph extent): a block is one `BAND` panel with a `LOW` border from 6 px left of the text (in the gutter) to the body's right edge across its lines, and consecutive quote lines are one `LOW` bar 4 px wide, 8 px left of the text (`view_tests::a_code_block_is_one_panel_and_a_quote_one_bar`, over widths and two scales, compared after the clip to the body; `a_copy_of_a_code_block_is_its_text`). Limits: neither indents its text, so a wrapped line of either starts at the text's left edge, and a list shows as Slack's text carries it (`• `, `1. `) with no hanging indent. These are the drawer's reading of Slack's rules, not yet compared with the desktop app side by side (the look plan's premise PB, unrun). The parse is linear in the text, which any member can make one long line of openers: each line's end is found once and a failed closer scan is remembered per marker and line, since a closer for a later opener is one for the earlier (`mrkdwn_tests::the_parse_is_linear_on_a_line_of_openers`, which counts the scans, fence scans included, over lines of openers, fences, quote markers and tokens), CR LF and CR become LF, tabs stay, any other C0 or C1
  control becomes U+FFFD, and a name or file name loses exactly the C0 and C1 controls and the characters `page_reply::unsafe_char` lists (line separators, direction and zero-width formatting characters, U+2060 to U+2069, U+FEFF, U+FFF9 to U+FFFB, U+FFFE and U+FFFF), so no body line can read as a header (the header ends its own line). A token's label is one line with no control (a line end becomes a space, any other control U+FFFD; `project_tests::a_label_is_one_line_and_holds_no_control`), and a mention's, channel's or group's name also loses the `unsafe_char` list, an empty user or channel name showing the id (an empty group label shows no name and stays a `Markup` span). A record that starts a local day (its day is not the day of the record before it among those held, and the oldest record held always starts one) leads with a divider, one line of a `Day` span: `Today`, `Yesterday`, or the weekday, month and ordinal day (`Monday, November 9th`, with `, 2025` after it for another year), one function (`day_word`, with the thread summary's short form) deciding by the local dates; a stamp that is not a time is its own text, and the divider's words come from the stamp and the clock alone, so no body or name produces one (`project_tests::the_divider_says_today_yesterday_or_the_date`, `a_day_starts_with_its_divider_and_the_header_shows_the_time_alone`). A divider always comes with a header (a new day is not a continuation). The header is the name, the badge if any, then the time alone, `h:mm AM` whatever the day (the day is the divider's): the badge (`APP` for a bot, `DRAWER`, `ROW`) is a span of its own kind drawn from the record's `origin` alone (`the_badge_comes_from_the_origin_and_no_text_draws_one`), so no name or body can produce one. A thread's summary is the record's meta line of its reply count, `2 replies` (`1 reply`, a `Replies` span), and with `latest_reply` the `Last` span `   Last reply today at 2:44 PM`, `yesterday at …` or `Nov 9 at …` (the year after the date for another year), by the same `day_word` (`project_tests::the_thread_summary_reads_as_slacks`). A record continues its author's group, and has no header, when `project` is given the record before it among the held records and both have one author id, name and origin, one local day and times under `GROUP_GAP` (300 s, in whole seconds) apart (`continues`; the oldest record held always starts a group; a tombstone groups like any record, and its notice follows the group alone; `project_tests::a_message_continues_its_authors_group_within_five_minutes_on_the_same_day`); a continuation is body and meta spans alone, so a body that imitates a header is still body text (`a_body_that_imitates_a_header_draws_no_tile_and_no_time`). `GROUP_GAP` is one constant resting on Slack's rule as remembered: no published source states it (look plan, premise PA), and only a person's test in the desktop app can confirm it. The time is added to the stamp as integers (`when`, which only the compose line's `reply in thread of` uses now, is `h:mm AM` for today's local date, `Nov 9 2:14 PM` for an earlier day of the year and with the year for another; `project_tests::a_time_is_twelve_hour_as_slack_shows_it`; a stamp out of chrono's range shows as its own text, never a panic).
  Limits: a record's spans depend on the record before it, so the buffer cache's key changes when a neighbor does, and a page that joins above the top record can take that record's header off, which moves that record's text and every record below it up by its header and its divider (a continuation of the page's last record carries neither), under a fixed anchor, and the reverse: the oldest held record always starts a group and a day, so when the oldest leave (at the 276 ceiling) the new oldest gains a header and a divider and the records below it move down by both (`view_tests::a_page_above_moves_every_record_below_it_by_its_header_and_divider`); one fixed offset serves every stamp, so a post from before a daylight saving change shows an hour off, and the day a group or a divider ends at is that offset's, which near a change can put `Today` or `Yesterday` an hour from where the person's clock puts it; a name made only of removed characters shows empty (a file name so made shows `[a file]`); what stays in a name includes the soft hyphen, tag characters, U+180E and U+206A to U+206F, none of which breaks the header line.
  An edited message (`ChatRecord::edited`) ends its body with an `Edited` span, `(edited)` at 12/15 in the muted color, after a space on the line of its last word, and on its own line after a block or a quote (inside the panel it would read as code), before the meta lines; a deleted message shows none, and no typed text becomes an `Edited` span (`project_tests::an_edited_message_ends_with_edited`). The copy leaves it out, because it is Slack's mark and not the poster's text: its span carries a glyph flag (`EDITED_RUN`), `shown_line` cuts a line at the first span with it (the space before it goes too), and the marked message's copy drops the line it is alone on, so a drag over the line, and the marked copy of a one-line message, a block or a quote, give the poster's text (`view_tests::a_copy_leaves_out_edited`); a message with nothing else has nothing to copy. Its look is pinned in `view_tests::the_header_is_the_bold_name_and_a_smaller_muted_time`.
  `view.rs` is the view (`ChatView`, with `ChatDrawer::layout` and `areas`): `place` puts the records in the drawer's body, upward from the newest at the
  bottom while following (the anchor is `None`) and downward from the anchor otherwise, and asks a record's height only when that record reaches the body. A record is shaped by
  `set_buffer_rich_text` from `project`'s spans, each kind with its own style (`attrs`: the name's, the time's and the badge's differ from the body's, so a body line that imitates a header looks like a body). The time is 12/15 and the badge 10/15 of the body's size, and each keeps the body's line height in its metrics, so the header line is as tall as a body line (cosmic-text takes a line's height from the glyphs that set one; `view_tests::the_header_is_the_bold_name_and_a_smaller_muted_time`). A divider is a lead line on its own line height (`DAY_PX`, 30 px at text scale 1), its label 13/15 of the body's size and bold, centered in the text's width by the line's alignment (so the pill sits half the gutter right of the body's middle); its label's run, with its padding of three no-break spaces each side, carries the `DAY` glyph flag, and `backs` draws around it a pill a body line tall (a `LOW` border and the ground a pixel in) and a 1 px `LOW` rule from the tile's left distance to the pill and from the pill to the same distance from the right edge, so none is text (`view_tests::a_divider_is_a_pill_with_a_rule_to_each_side`); the mark's band and bar and the group's tile start below the divider's lines (a drawer too narrow for the label wraps it into more than one line, each with its own pill, and `Shaped` takes the divider's height from the shaped lines, so nothing assumes one), and a group's first record is at least as tall as its divider and tile. A thread summary's spans are 13/15, the count bold in the accent and the rest muted, on a line of the body's height (`a_thread_summary_is_its_own_line_in_the_accent_and_the_muted_color`). A gutter (`GUTTER_PX`, 52 px at text scale 1) lies left of every record: the buffers are shaped at the width right of it, and the areas, the selection's lend, the press and a glyph's place all take the text's rectangle from the one function `text_rect`, so a press in the gutter selects nothing. A group's first record draws a 34 px square tile in the gutter, colored by an FNV-1a hash of its author's id and name over eight fixed colors, from the author fields alone and never from text; it carries no letter (a letter needs a second text area per header, which waits with avatars; `view_tests::a_tile_starts_each_group_and_no_text_draws_one`); a group's first record is at least as tall as its tile, so a header alone (an empty text) does not run the tile into the next record (`view_tests::a_header_alone_is_as_tall_as_its_tile`). A marked continuation shows its time, `h:mm`, at 11/15 of the body's size in the muted color, right-aligned in the gutter, as one small buffer the view keeps and shapes again only when its text or the scale changes (`the_marked_continuation_shows_its_time_in_the_gutter`). Each `Shaped` records its lead (the divider's line if any and the header's if any: 2, 1 or 0) and the divider's height; a press selects from line `lead` on, a drag's end never goes above it, and the marked message's copy skips `lead` lines (`a_continuations_first_line_selects_and_a_header_never_does`, `the_marked_text_skips_only_the_lead`, `a_divider_is_neither_selected_nor_copied`). The palette is one table of constants in `view.rs`, from Slack Kit's dark mode chart; the contract holds none of its values, and the view tests assert them. The view's rectangles (`ChatView::backs`) are `CellQuad`s of the cell backgrounds' palette, which paints first in the pass: the background of the drawer's inside (body and compose foot), then per placed record its mark band and bar (2 px at text scale 1, scaled with the text: `the_marks_bar_follows_the_text_scale`), the group's tile and its badges, and last the selection; all but the background are clipped to the body, and none is drawn while the chat is hidden (`view_tests::every_rect_is_inside_the_body_and_the_background_covers_the_drawer`). A badge's rectangle is its run of glyphs (`render::text::flagged_runs`, over the `BADGE` glyph flag, tested by `text_tests::flagged_runs_are_found_mid_line_at_a_lines_end_and_across_a_wrap`; a run is the extent of its flagged glyphs that are not a space, so a right-to-left stretch, whose glyphs descend in x, has its rectangle and the space that ends a wrapped line, which can hang past the width, has none: `text_tests::a_flagged_run_covers_its_glyphs_in_a_right_to_left_line_and_leaves_out_a_trailing_blank`), and its padding is two no-break spaces each side, glyphs of the run, so it scales with the text and its left padding is whole where a wrap starts a line with it (`a_badge_keeps_its_rectangle_and_its_padding_where_a_wrap_cuts_the_header`). `ChatDrawer::layout` returns the rectangles of the frame it laid out, and `layout_drawer_px` hands them to `redraw`, which adds them to the chrome's cell backgrounds and builds the palette then, so no rectangle can be the last frame's (`the_layout_returns_this_frames_rectangles`). Inline styles are `styled_attrs`: bold and italic are the face's weight and slant, strike carries the `STRIKE_RUN` glyph flag and code a 14/15 monospace face in orange (the one color not from the chart) with the `CODE_RUN` flag, both on the body's line height; a mention carries the `MENTION_RUN` flag and `backs` draws behind it a `SLUG` fill (the chart's Sky Blue at about 10% over the background), 1 px wider each side than its run and inset like code's, no border (`view_tests::a_mention_draws_its_slug`: user, channel, `@here` and a group wrapped at every half pixel of width from 100 to 140, marked and not, at two scales, the slug cut at the body's edge and under the selection; a link has no rectangle); `backs` draws behind a code run a `LOW` border and a `BAND` fill a pixel in, padded 3 px left and right and inset 2 px above and below, and through a strike run a 1 px rule in the text color at the line's middle, both from `flagged_runs` over the flags, so no text draws one without the styles (`view_tests::code_and_strike_draw_their_rects_from_glyph_flags`, a table over short, wrapped and marked runs, one border, fill and rule per layout line a run spans; `a_drag_across_styled_runs_copies_the_shown_text`; typed code that reads as a badge draws a code box and no badge, `the_badge_comes_from_the_origin_and_no_text_draws_one`); the buffer holds the shown text, so a selection and the marked message's copy give it without markers, a link's label or a mention's name, never a link's URL beside its label (`view_tests::a_copy_gives_the_shown_text`). Limits: a code run's fill is the mark's band's color, so on a marked record only its border shows; a style does not reach across a token's boundary rule (`<@U1>*b*` is literal); an outer style around code (a strike pair around a code run) is dropped, so the code shows alone; a token holding a line feed is one atom, so a style can span that break; a struck link draws the text color's rule, not the link's; the slug is not visible on a marked record (the band and the slug are nearly one color, 1.04 to 1); in a right-to-left paragraph a mention whose `@` and name the bidi order parts around other text draws two slugs; mentions that touch share one slug, since a slug is drawn around a run of flagged glyphs; a link is not underlined and nothing is clickable (a click that opens links would need the URL shown first); a badge is a rectangle with square corners; where a wrap ends a line right after the badge's word, the wrap drops the padding after it (the rectangle still covers the run); the selection's tint (the chart's Sky Blue at 30%) is chosen here, not read from the chart.
  and its buffer is cached by those spans, the width and the text scale; the buffers in view are kept plus up to 64 others, the farthest from the position first. The records
  that cross the body are prepared as clipped `ExtraArea`s appended to the frame's one `extras` vector, before its one `prepare_text`. `no messages` is drawn
  only when the owner's fill is `Complete` and it holds no record (`ChromeView::chat_empty`, `ChatOwner::no_messages`; not while reading, offline, in a gap or unsupported:
  `view_tests::no_messages_shows_only_for_a_channel_read_whole_and_empty`; the line is drawn only while Chat is shown and `chat_empty` is set, `chrome::view::tests::the_chat_drawer_draws_no_messages_only_when_told_the_channel_is_empty`; `draw_chrome`'s passing the flag from State is not asserted by the native fixture, which never shows an empty channel). The order is pinned by `view_tests::the_view_shows_records_oldest_to_newest_from_an_owner_whose_records_arrive_out_of_order`, the cache's farthest-first bound and the top clip by
  `the_view_shows_the_held_records_newest_at_the_bottom_and_shapes_only_what_it_shows`. A reader scrolled up stays put: the anchor is `None` (following the newest) or `(ts, dy)`, the top record's time and its top `dy` below the body's top, so an arrival, an edit that changes any record's height, or a delete elsewhere moves nothing (but see the limits). PageUp and PageDown (`ChatView::scroll`, `Step::Page`) move a third of the body, Alt+Up, Alt+Down and the wheel one line (`LineScroll::Chat`, `chat_wheel`), and `chat.latest` (End, `Scope::Chat`) sets `None`; they act only while the drawer is Chat (`chat_move`, `chat_wheel`), the REPL's Alt+Up and Alt+Down unchanged. Each frame finds the anchor by its time among the held records (an anchor no longer held passes to the nearest newer record at the same `dy`, so a snapshot that replaced every record does too, and with none newer the view follows), then `settle` takes up the records above the top while there is room, drops those wholly above it and makes a position whose newest record is wholly in view `None` (`view_tests::an_anchored_reader_stays_put_through_arrivals_edits_and_deletes`, `the_keys_and_the_wheel_reach_the_view_only_in_the_chat_tenant`). A row change returns the reader to following (`a_row_change_returns_the_reader_to_the_newest`). Keys and the wheel are kept as line and page counts and turned into pixels by the frame that spends them. Alt+Up and Alt+Down reach the chat through `line_scroll_target` (focus in the drawer, Chat shown). Limits: a reader parked on the oldest held record stays on it while the store passes 256, but only to 276 (`CEILING`); past that the oldest go and the reader moves by the rule for an anchor no longer held; a delete or an edit that brings the newest wholly into view returns the reader to following, which can move the text by at most the height lost; a record taller than the body shows its end.
  The message mark (B5o): `ChatView::mark` (`Option<Ts>`, the marked record by its time) is always a held record in view. `chat.mark_older` and `chat.mark_newer` (ArrowUp, ArrowDown, `Scope::Chat`, so every other pane keeps its ArrowUp and ArrowDown and Alt+Up and Alt+Down stay the scroll's) are queued in order by `ChatView::mark` and spent by the next frame in `ChatView::layout`, which folds them one at a time, each with its clamp (limit: keys pressed before one frame apply the scroll first and the mark keys after, so a mark key then shows the mark where it was, not where the scroll left it); with no mark a key marks the newest, `mark_newer` on the newest and `mark_older` on the oldest stay. A mark moved out of the body moves the position: a record crossing or above the top puts its top at the body's top, one crossing or below the foot its bottom at the foot (the newest wholly in view gives `None` by `settle`), one taller than the body shows its top. "In view" is the record's first line inside the body (top at or below the body's top, its one line at or above the foot, measured from where its band starts, below its divider): a frame that leaves the mark's first line outside the body (a scroll, an arrival, a resize) moves it to the nearest placed record whose first line is inside, and when none is (a tall record crosses the top and the next one's first line is below the foot) to the first placed record, the one crossing the top, which covers all of the body but at most one line (`view_tests::the_marks_first_line_is_inside_the_body`, `mark_keys_fold_one_at_a_time`). Each frame finds the mark by its time as the anchor is found (one function, `find`), so a mark no longer held (a resync, the trim; a live delete keeps a tombstone held) passes to the nearest newer held record or to none, in that one place. A row change clears the mark with the position (`ChatView::reset`, from `ChatDrawer::tick`); End does not, and the frame then moves the mark into view. The mark is drawn as a full-width band with a bar at its left edge (`backs`), not as a style of the record's spans, so it is no part of the span cache's key and a move restyles and reshapes nothing (`view_tests::the_mark_is_a_band_and_moving_it_keeps_the_selection`, `the_mark_is_a_held_record_and_in_view`, `a_row_change_clears_the_mark`, `the_keys_and_the_wheel_reach_the_view_only_in_the_chat_tenant`). The draft edits on typed text, Space, Backspace, `chat.newline` and paste only, so the arrows never edit it.
  The thread view (B5p): `chat.thread_open` (ArrowRight, `Scope::Chat`) opens the marked message's thread (`ChatDrawer::open_thread`, only with a mark and no thread open): `thread.rs`'s `Thread` keeps its own copy of the parent (with its deleted flag, so a deleted parent shows `(message deleted)`), the replies and the channel's parked `ChatView` (position, mark, shaped buffers), which returns at close; `ChatDrawer::thread` is the one switch for which collection the drawer shows, `ChatDrawer::layout` places the thread's records instead of the channel's, so the thread has a view of its own and the same view code. The thread is read whole: `chat.history {target, thread: parent, limit: 20}` and its `next` cursor, one read at a time through the reads-in-flight set; a reply is taken only while it equals the read in flight for that host (replies are matched by value, so one for a closed thread or another's changes nothing), and only records whose `thread_ts` is the parent's join, sorted by time with repeats by `ts` dropped (within a page and across pages), the parent once, as the thread's copy. A thread closed and opened again while its first read is in flight waits for the old read to leave the set, and the old reply fills nothing (`close_thread` abandons it). A refused read is asked again after `PACE`, or `LIMITED_PACE` for `limited`, by `tick`, and the title names the code. The thread holds at most `HELD` = 156 records, parent included, and shares the drawer's 256 with the channel: `ChatOwner::reserve` tells the owner how many the thread holds, the channel's store keeps `KEPT - reserved` (so its oldest give way, its newest 100 never), a channel page in flight joins only within that share (`a_reserve_takes_room_from_the_channels_oldest_and_gives_it_back`, at the owner), and while a thread is open the owner holds no anchor and asks for no page. Past `HELD` the walk stops and the title says `the rest of this thread is not shown` (the newest replies are not shown). A channel snapshot never touches the thread's replies. A restart (a link's return, or the reread after a post) clears the replies and shows the parent until the first page returns: a limit. Escape closes it first (`drawer/keys.rs`, then focus returns to the nav on the next Escape); a row switch (`tick`'s row change) and hiding the drawer (`hide`, also Help opened over the chat) close it; a link's end leaves it open while the re-resolve finds the same channel (another channel, unmapped or a refusal closes it, as the watch stops) and its walk starts again at `Connected`. The title reads ` chat · <channel> · thread `, then the thread's words (a refusal, `this daemon has no chat: update it` for an unsupported reply, the cut), then the words of the link and the watch (not connected, a gap, a refusal), which stay over a thread, each phrase separated by ` · `; the channel's older-pages words and `no messages` are not shown while a thread is. A draft begun in an open thread is that thread's: it posts there with the channel as its target, its compose line reads `reply in thread of h:mm AM` (the parent's time by `project::when`, with its date before it for a parent from another day), and the destination survives a close, a row switch, a reconnect and the thread closing with its channel's watch (the draft's own thread wins in `Compose::edit`: `thread_tests::a_draft_posts_to_the_destination_it_was_begun_in` drives the close, the reconnect, a row switch and a refused re-resolve; the thread's own closing by a row switch and a watch change is `thread_tests::thread_view_compose_and_back_keep_target` and `a_thread_closes_when_its_channels_watch_changes_or_stops`); with no draft the line names where the next would go (the open thread's parent), and a channel draft stays the channel's when a thread opens (`thread_tests::a_draft_posts_to_the_destination_it_was_begun_in`). After the person's own `posted` for a post into the open thread (`Thread::is`, by the Slack thread's identity: host, workspace, channel and parent, whatever row posted; only a `posted` reply, `thread_tests::{only_a_posted_reply_rereads_and_an_empty_draft_names_the_open_thread, a_thread_is_the_one_posted_to_by_host_workspace_channel_and_parent}`), `ChatDrawer::restart_thread` reads it again whole, from its first page (the one restart, also the link's return: it gives up the read in flight first, so an equal reply still coming is dropped and the reread goes out behind it, `thread_tests::a_post_while_the_first_page_is_in_flight_is_read_again_once_it_is_answered`), so the replies are cleared until the first page returns (CHAT-THREAD-REREAD-CLEARS); a post to the channel or to another thread reads nothing. A thread lives only under its channel's watch: `owner_outs` closes it when the watch's target changes or the watch stops (a re-resolve to another channel or to unmapped). Every give-up of a thread read is explicit: `give_up_read` (the link's end, and the first step of `restart_thread`) and `close_thread` abandon the read in flight, so a read held behind an equal one is cancelled and its reply dropped, and `thread_outs` gives up nothing (`thread_tests::{a_reply_repeated_in_a_page_or_across_pages_shows_once, a_thread_closes_when_its_channels_watch_changes_or_stops, a_thread_read_held_behind_an_equal_one_is_cancelled_by_a_link_change, the_threads_title_keeps_its_phrases_apart_and_says_an_unsupported_reply}`). At close the channel's mark is on the parent, or, if a resync dropped the parent, the view's one `find` passes it on (`thread_tests::{thread_view_compose_and_back_keep_target, a_thread_is_read_whole_and_its_replies_match_by_value, a_thread_shares_the_bound_with_the_channel_and_says_what_it_leaves_out}`; Escape's route through `drawer/keys.rs` rests on reading).
  `compose.rs` holds the draft (`Compose`, one `Draft` per canonical row; `compose_tests::the_draft_keeps_its_destination_and_its_escaped_bound`). It holds only what wire's `post_text_fits` admits (the escaped form within 8 KiB, so the post reads back whole): a typed key or a paste past it is refused whole, with a status line, and the draft is as it was (an oversized first paste begins none). Its destination (target and thread) is the channel watched, and the thread open if any, at the first character and kept through any change of view; an emptied draft is dropped and follows the view again. Typing and pasting make CR LF and CR into LF, keep tab and drop every other control character (C0, DEL and C1), which is exactly what the drawer cannot show. `draft_edit` says which key edits it: a typed character or Space only where `chord_types_text` (`input/mod.rs`, also the editor's) says the chord types (AltGr does, Ctrl+a does not) and only when the key resolved to no action, Backspace (also with Ctrl) deleting one character, and `chat.newline` (Shift+Enter); `chat.send` (Enter, Chat scope) posts. In the Chat tenant (`DrawerInput::Chat`) `chat_key` sends a key or a paste to `ChatDrawer::edit`, which is given the row the chat shows at that key (`wanted_row`), not the last frame's, and begins a draft only in the watched channel if that watch is that row's (`compose_tests::the_wired_draft_is_the_shown_rows_and_begins_in_its_own_watch`); a refusal goes to the status line. A listing's retired rows drop their drafts (`ChatDrawer::retire`, called from `reconcile_pending_results`). The compose rows sit at the foot of the drawer (`compose_rows`, over `ComposeText` that `ChatDrawer::compose_text` builds once a frame and only while Chat is shown): `> ` and the draft's last three rows, each line of the draft (through `cell_lines`) wrapped to the drawer's width with `wrap_status`, a cursor block ending the last while the drawer has focus, then its destination (`to <channel>`, or `reply in thread of h:mm AM`) with how many rows are not shown, so at most four; in a drawer shorter than that it keeps one draft row fewer than its height, counting the rows it dropped, and the destination is the last dropped; none while no channel is known (`compose_tests::the_compose_rows_show_the_draft_wrapped_to_the_drawer_and_counted`, which also asserts the draft's exact text; `chrome::view::tests::the_chat_drawer_draws_its_compose_rows_at_its_foot_and_no_other_drawer_does`). The chrome draws them and reports their number (`ChromeView::chat_foot`, kept by `ChatDrawer::set_foot`), and the records' body is that many rows shorter (`layout_drawer_px`). Limits, each needing a window: the keys hook's paste branch (the clipboard read), `chat_key`'s `last_key` and `edit_chat_draft`'s status line, the call from the listing to `retire`, the editor's call of `chord_types_text`, the records' body being shorter by the compose rows (`layout_drawer_px`), the cursor block shown only while the drawer has focus, and the compose text built only while Chat is shown (both in `draw_chrome`); the draft is kept in memory only.
  Posting (B5m): `chat.send` (Enter) reaches `chat_key`, which calls `State::send_chat_draft`, then `ChatDrawer::send` with `Settings::chat_name` (`[chat] name`; unset or blank is `None`, the one check of the name, and the window holds no copy of the door's rule). It sends one `chat.post` (the draft's target and thread, `kind: person`, the name, the plain draft as `text`, which the window never escapes: the door does) only for a non-blank draft, with no post waiting and no `limited` wait running (`compose_tests::compose_retains_refused_limited_and_unknown_drafts`). While it waits the owner holds it, the draft refuses edits and a second Enter with `posting…` on the status line, and only `ChatOwner::on_posted` for the post it awaits, when the reply is `posted`, returns the row whose draft the drawer clears. Every other outcome keeps the draft and says why on the status line: the owner reports each outcome it takes (`take_report`, so a repeated one is said again) and `ChatDrawer::take_notice`, taken in `with_chat`, carries the words. A refused `bad_request` names `[chat] name` (the window checks `post_text_fits`, so the name is the door's reason), `limited` says the seconds left (`about` while the wait is a guess) and the owner refuses Enter until it ends, `unknown` says to check the channel before sending again, and `PostNotSent` (fe-net: the connection's hello did not say `posts_plain_text`) says the hub must be updated. Enter and Shift+Enter resolve in the Chat alone to `chat.send` and `chat.newline` and keep every other pane's action, and the Chat border names Enter (`keybindings_tests::enter_and_shift_enter_resolve_in_every_context_and_the_chat_border_names_them`). A post `send_to` refuses is reported to the owner as `Refused(Unavailable)` by `with_chat`. The native fixture pins Enter's resolution in the Chat pane and `send_chat_draft`'s one `ChatPost` (thread `None`). Name prompt: Enter with a draft and no `Settings::chat_name` makes `ChatDrawer::send` open `asking` (sending nothing, the draft untouched), and `name_key` (before the reading keys and Escape in `drawer_pane_key`, only while asking) sends keys to `edit_name`, Enter to `State::save_chat_name` and Escape to `cancel_name`; the foot is one row, `Name for your chat posts: <name>`, ending in its tail (`ComposeText::asking`; `compose_tests::enter_with_no_name_asks_for_it_and_sends_nothing_until_the_next_enter`). The prompt's lifetime is the draft's view: each frame `tick` cancels it while the row is not mapped (a changed row, a hidden drawer, no mapping), and `hide` and a link change cancel it, the draft kept (`compose_tests::the_prompt_ends_where_the_view_it_asked_in_ends_and_keeps_the_draft`); reading keys (scroll, mark, thread) still act while it is open. Keys are routed by the pure `name_step` (`name_step_says_what_each_key_is_to_the_prompt`) and the save by `apply_name` (`apply_name_saves_closes_the_prompt_and_the_next_enter_posts_under_it`, on a temporary settings file); `edit_name` refuses a paste with a control character and a name past `MAX_NAME` bytes whole. A refused name or a failed write goes to the status line and the prompt stays open. Name write: `persist::chat_name::save_chat_name` checks the name (`check_chat_name`: trimmed, not empty, no control character, no quote or backslash so the quoted string reads the same in the window's parser, which does not unescape, and the daemon's strict one, and within wire's `MAX_NAME` bytes, which is all the daemon's post refuses), then writes `[chat] name` into `Settings::source` (the file loaded) or platform's per-user `settings.toml`, making the folder, with `write_durable`, every other byte kept (`with_chat_name`: an existing line replaced in place, with its comment and line end but not its indentation, else under an existing `[chat]`, else a new table at the end; a line it adds ends in LF; a leading byte order mark stays), and sets `chat_name` at once. The strict reader is the truth (`checked_text`): the new text is written only when `toml` parses it and its `chat.name` is the name, so a file whose `[chat]` is spelled some way the line scan does not see (quoted, dotted, inline), that names `[chat]` or `name` twice, or that is not valid TOML is refused with words that say how to add the name by hand, and nothing is written. A symlinked file is written through to its target and stays a link; an unreadable file, or one that is not UTF-8, is left as it is; a failure leaves the file and the setting as they were (`chat_name::tests`, each of these a test). The file is the one the window loaded, which may be a `.sot/settings.toml` found from the working directory, so a name written there is read at the next start from that file. Limits: two windows saving at once on one home: the last write wins and a write racing another can fail (platform's `write_durable` names its temporary file by the target); the window's own reader (`merge_text`) is a lenient line scan and is not the strict one; and, each needing a window or a key event: `name_key`'s glue (its order before the reading keys in `drawer_pane_key`, the clipboard read, `last_key`), `State::save_chat_name`'s status line, `chat_key`'s own branch for Enter, the status line showing the words, and `with_chat`'s handling of a post `send_to` refuses (`mod.rs`), which only State reaches and no test drives, and the window wake of fe-net's `answer_locally` (the steady tests' window ignores redraws).
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
