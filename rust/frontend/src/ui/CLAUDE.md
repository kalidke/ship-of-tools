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
`preview::requests::PreviewRequests` owns both request clocks, the fired preview target and its page re-raster, concept
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
`preview::shaped::ShapedPreview` owns the flowed preview buffer, its rect, scroll and definition-line anchor, the math
SVG, the refused-hello overlay and the highlighter; `preview::image::raster::RasterView` owns the shown raster, its
zoom, pan and dimensions, the visible ROI and the same-directory view carry; `preview::image::pages::PageView` owns
the shown preview's page, its raster zoom and a re-raster's keep-view; `preview::image::figure_overlays::FigureOverlays`
owns the scalebar toggle and label, the caption store and label, and the caption band the last frame reserved;
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
cell grid and the cell metrics; `render::brushes::Brushes` owns the quad pipeline, the solid brushes, the logos and
the help peek's backing; `app::startup::StartupScript` owns the harness's command-line one-shots;
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
  source's node), the in-flight page re-raster, the crop host pin, the scale-save marker and the one `--roi` aim. A
  new cursor target and a page turn cancel a pending re-raster; an admitted reply ends it, keeping the view only when
  it shows the pending page. Crop still takes its host pin before comparing, so a wrong host consumes it and two
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
- `ShapedPreview` privately owns the pane's flowed text: one buffer shaped for the rect the last layout pass gave it
  (resized only when the rect moved by more than half a pixel), the math SVG beside it, the scroll in body lines, the
  definition-line anchor and the refused-hello overlay. Installing a buffer drops the SVG and scrolls to the top. A
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
  resets the raster zoom to fit. `FigureOverlays` privately owns what is drawn over a figure: the sticky scalebar
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
  the scroll is clamped each frame to the widest table. A math SVG rasterizes once, at the pane's size.
- `ControlChannel` privately owns the window half of the control channel (ADR 0019): the queue the command-file
  watcher fills through its producer end, drained whole under the lock alone so dispatch never holds it, and the
  signature of the last `fe-state.json` written, so the file is rewritten only when the state it reports changes.
- `ReplyBar` privately owns a reply a page offered: the daemon it came from, the row's id and listed name, the handle
  and the exact line, and what the person can see of the bar (covered by a dialog, cut off by a short window, or
  whole). It never takes `State`. At most one offer is open: a second is dropped and the status line says so. `State`
  (`control/reply_dispatch.rs`) opens the bar only for a row `RowDirectory::resolve` finds listed on the offering daemon
  by its exact id (else the offer is dropped and said), requests the user's attention, tells the bar what is drawn over
  it before each key and with each frame's overlays, and applies what it returns. A line that is not exactly
  `page_reply::check_line`'s opens no bar, so the panel shows what will be typed. The bar is the only sender of
  `page.reply`: Enter returns a `ReplySend`, a value only the bar can make, and `OutgoingReq::PageReply` is made from
  nothing else; `FeCommand::PageReply` is `skip_deserializing`, so the command file cannot open a bar, and
  `route_fe_command` makes it only from a directed `fe.command`, so a broadcast opens none. Enter sends only once the
  bar has been wholly in view for one second and is not a repeat; Esc drops it; while the bar is in view it takes every
  key, plain Enter and Esc by their logical keys and never a binding. Keys follow what is drawn on top: while the close
  dialog (the quit question, a leave's line, a stayed relaunch's notice or the quit hint) is drawn it takes every key
  and the bar none, and the wait starts again when the bar is back in view, so no Enter given to a dialog or typed
  while the bar was hidden sends a reply. A window too short to draw every row of the line can drop the bar but not
  send it. The panel is the close dialog's renderer (`CloseDialogText::Reply`), titled "Reply from a page", under the
  close dialog; the target (handle, listed name, host) and the line go through `cell_lines` and `wrap_status`, so a
  hostile name stays on rows of its own with no control character in a cell. The mouse takes no part. The answer comes
  back as `IncomingEvt::PageReplied` and `page_reply_status` puts it on the status line: only `sent` says the line
  and its Enter were recorded. Behavior tests: `control::reply_bar::tests::{the_bar_sends_only_on_an_armed_enter,
  esc_drops_and_sends_nothing, the_bar_takes_every_key_while_open_and_in_view, a_second_offer_while_one_is_open_is_dropped,
  an_offer_whose_line_is_not_the_daemons_marked_text_opens_no_bar, the_bar_shows_the_target_and_the_exact_line,
  a_window_too_short_for_the_line_can_drop_the_bar_but_not_send, a_dialog_over_the_bar_takes_every_key_and_the_wait_starts_again,
  the_waits_deadline_is_only_while_the_bar_is_waiting}`, `control::command::tests::{a_directed_page_reply_opens_the_bar_and_a_broadcast_does_not,
  a_command_file_cannot_open_a_reply_bar}`, `input::keypress::tests::the_quit_dialog_takes_a_key_before_the_reply_bar_and_the_bar_before_the_panes`
  and `control::replies::tests::the_status_line_says_what_became_of_a_confirmed_reply`; the native State fixture
  `the_bars_enter_sends_page_reply_to_the_pages_host` (a reply from host B's daemon opens the bar, every other command
  and a second offer send nothing, an Enter before the wait sends nothing, and the armed Enter puts one `PageReply`
  with the line on B's connection and none on A's) runs in the release gate with the other State fixtures.
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
  notice, a capture's result, an interrupted upload, the fullscreen pin) also holds it for `NOTIFY_STICKY`, during
  which `rebuild_connection_status` leaves it; the idle tick expires the hold once and rebuilds the line.
- `BatteryGauge` privately owns the battery readout: the OS is asked at most every 30 s, the label is `None` with
  no battery or on an error (never a fake reading), and every frame between queries reuses it.
- `Pointer` privately owns the pointer between events: the last position (winit reports it only on motion) and the
  wheel's fractional rows, stored by the wheel adapter and cleared when the agent pane consumes a gesture.
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
  when it decoded) and the help peek's backing, rebuilt only when its alpha changes. Painting is its operation over
  the `Gpu`; `Gpu` also holds the surface format's clear colour.
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
- `LayoutHelp` privately owns drawer/maximize/wide flags, Help browsing, the first return origin and startup
  requests, including private nested storage. It returns owned effects, never takes State and lends only immutable
  observations. Ordinary toggles alone abandon the origin and request Monitor sync, after focus and before binding
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
`OutgoingReq`, `IncomingEvt`, `HostTable`, `lane_dial`, `ResolvedDial`, `--socket`, `--dial`, `--relaunched`,
`relaunch.request`, `spawn_watcher`, `rust/frontend/src/relaunch.rs`, sot_protocol::annotation::split_frontmatter, sot_protocol::annotation::synced_against, sot_protocol::physical_scale::PhysicalScale, sot_protocol::physical_scale::parse_physical_scale, sot_protocol::video_path::video_mime, lease_notice, handle_fe_command_send relay diagnostics.

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
- `drawer/terminal/`: the Terminal tenant (local pty, vt100 helpers).
- `preview/`: PreviewRequests owns request clocks, the fired target, concept follow, driven holds and the crop,
  save and aim transactions; InstalledPreviewSource owns the installed source; State coordinates preview installation,
  shaping and transport (fetch, replies, layout, concept, open).
- `preview/image/`: bitmap previews as quads: figures, overlays, view, ROI.
- `preview/markdown/`: markdown and source text shaped into cosmic-text buffers.
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
