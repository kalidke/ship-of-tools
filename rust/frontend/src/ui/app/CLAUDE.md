# rust/frontend/src/ui/app: the winit application (fe-ui)

The window's application shell: `App` holds the window's `State` and its startup inputs, and the winit callbacks run
the event loop against it. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md. ADR 0050 describes the exit and
hand-over it serves.

## Files
- `mod.rs`: App, its constructor and run/finalizer boundary, plus FRAME_BUDGET and NAV_FIRE_DEBOUNCE.
- `exit_process_tests.rs`: isolated bounded-runtime shutdown cases using the actual App finalizer.
- `exit.rs`: the Ctrl+Q prompt's key table, `request_quit`, `leave` and `finish_exit`, and `redraw_exits`.
- `handler.rs`: `impl ApplicationHandler for App`: `resumed`, `window_event`, `about_to_wait`, `new_events`.
- `frame.rs`: `State::redraw`, one frame's sequence, its upkeep (`frame_upkeep`) and `ack_presented_lines`.
- `tests.rs`: the native minimized-window event-progress harness; test-owned inputs, no daemon or user settings. It first runs the native-only State fixtures of the result-routing, badge and account commits, printing one `state-fixture name=... ok=...` line each.

## Start here
`redraw` in frame.rs for the order of one frame; `window_event` in handler.rs for which winit event goes where and for
the redraw arm; `keyboard_input` and `route_key` in ui/input/keypress.rs and the pointer functions in ui/input/mouse.rs
for any input change; `about_to_wait` for wake-up scheduling and the `[display] fullscreen_vsync_pin` setting;
`request_quit` in exit.rs for how the window closes.

## Rules
- Every user quit goes through `request_quit`: `exit_intent` asks on Ctrl+Q and leaves at once on the close button.
- begin_leave can access only lease and exit-state slots and returns Redraw or Finish; leave_close_keep_and_handover_only_leave_leases drives that production transition and observes pending exit and actual lease frames. No agent or drawer input capability is passed to it.
- A leaving window exits from `about_to_wait`'s poll once the acks are in (`redraw_exits`).
- A harness instance (`ephemeral`) starts neither watcher thread: `resumed` skips `relaunch::spawn_watcher` and `spawn_command_watcher` for it.
- Frames are capped at `FRAME_BUDGET` (`window_event`'s redraw arm and `about_to_wait`).
- A frame runs in `redraw`'s fixed order: upkeep, the chrome draw, the pixel layout, the text prepare, one render pass,
  then the capture's staging, submit, present, the acks and the capture's write.
- While the quit prompt is open, non-repeat Tab toggles and Enter confirms by logical key identity, every other non-repeat key cancels, and repeats do nothing; input routing consumes the event before modifier-only suppression and later dispatch.
- A second close resolves close_now's code and makes one bounded deliver_queued attempt before the window finishes; a write timeout is reported and is not a daemon acknowledgement.
- Every return from run_app, including an error or capture completion, takes the transport runtime once and calls shutdown_timeout(LEAVE_WRITE_WAIT) before App drops. Timed-out blocking work may continue; the yielding-child cleanup proof does not promise cancellation of arbitrary synchronous work.
- Native progress evidence separates producer workload validity from UI queue progress; counters are taken at successful fan-in enqueue and actual State dequeue, and the fixture never drains the queue.
