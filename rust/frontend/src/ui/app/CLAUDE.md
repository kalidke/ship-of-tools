# rust/frontend/src/ui/app: the winit application (fe-ui)

The window's application shell: `App` holds the window's `State` and its startup inputs, and the winit callbacks run
the event loop against it. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md. ADR 0050 describes the exit and
hand-over it serves.

## Files
- `mod.rs`: App, its constructor and run/finalizer boundary, plus FRAME_BUDGET and NAV_FIRE_DEBOUNCE.
- `exit_process_tests.rs`: bounded runtime and process-exit cases, run through the shared isolation helper.
- `exit.rs`: quit_prompt_step, request_quit, begin_leave, leave, finish_exit, redraw_exits and the per-App ExitDeadline shared with State.
- `handler.rs`: `impl ApplicationHandler for App`: `resumed`, `window_event`, `about_to_wait`, `new_events`.
- `frame.rs`: `State::redraw`, one frame's sequence, its upkeep (`frame_upkeep`) and `ack_presented_lines`.
- `native_eventlog.rs`: Windows only: the Application log's Application Hang events (1002) for one fixture child; an unreadable log is an error.
- `native_exit_tests.rs`: the opt-in main-thread native window-close fixture, using the actual App callbacks and test-owned inputs; the parent runs each close case as a child of its binary.
- `native_pane_daemon_tests.rs`: the pane_timing parent's private daemon: its own roots, rows created Ready or seeded with no supervisor, planted screen sentinels, and one window lease the parent holds for its whole life; teardown is that lease's Close (zero not ended) and the daemon's own exit, and a parent that dies without the Close leaves the daemon to end every row and exit.
- `native_pane_route_tests.rs`: the pane_timing routes: a `sotd stdio-bridge` stand-in, or two real ssh logins through a test-owned stand-in for the hub's relay unit; and the preflight that proves the route reaches the private daemon.
- `native_pane_tests.rs`: the opt-in native relayed-attach timing fixture: the parent runs three timing children and one fault child of its binary; each timing child switches once to every row through the real App and reports each attach's presentation receipt and the cells of that frame.
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
- The final window decision arms one three-second std-thread process backstop before a forced queued-write attempt or final event-loop exit; capture and returned-loop failure also enter this finalization. Prompt time and daemon acknowledgement/presentation time precede that deadline.
- Codes 0, 75 and 76 are preserved, including a Close superseding Handover; the historical immediate nonzero branch retains its foreground handover.
- Process-exit tests stall between the arm under test and every later arming opportunity: forced delivery stalls before finish_exit, a loop return stalls before fallback arming, nonzero finish_exit stalls before its direct exit, and fallback cases stall after fallback arming. Reversals remove only the production arm and must fail their own exit assertion.
- Ordinary native close must exit 0 before 2.5 seconds without the backstop; deliberate stalled teardown must end under the three-second backstop with the decided code. The native cases are OS close, Ctrl+Q then No, a second close during a held Close, a Close whose acknowledgement is held past three seconds and whose not-ended notice is presented, relaunch 75 and 76, capture through a real State, and closes held at the loop return, before delivery and before the direct exit. On Windows each ordinary case also requires no Application Hang event correlated to the fixture's image and process id.
- Native progress evidence separates producer workload validity from UI queue progress; counters are taken at successful fan-in enqueue and actual State dequeue, and the fixture never drains the queue.
- A frame consumes its local pane presentation candidate only after submit and present; an earlier frame error drops the candidate without completing the attach.
- A pane timing sample is the presentation receipt's own `since_request_ns` for a cold attach, counted only when that frame's drawn cells hold the row's planted sentinel and exactly one receipt follows the switch; every Ready sample must be within `CONNECT_BOUND`, and resume samples are recorded apart and not held to it.
- Under `test-pane-timing` only, `redraw` can fail the first frame that carries a presentation candidate before submit (the pane_timing fault child); that frame completes nothing and the next one completes the attach. Ordinary builds have no such hook.
- A pane_timing child ends when its parent's pipe to its stdin closes; the parent awaits each child and the daemon without panicking, and the kill-proof run kills a mid-run parent and requires every process it started to end and every row to be ended by the daemon.
