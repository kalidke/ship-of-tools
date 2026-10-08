# rust/log/src/attach_client/worker: the attach client's lane half (capsule)

One thread per client dials the supervisor lane, converges on its Ready, attaches over the attach lane, collects the
checkpoint and runs the steady state, reporting to the caller's event sink. Part of capsule; charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: constants and budgets, the message types (`WorkerMsg`, `WorkerEvent`, `InputOutcome`, `IngressReservation`), `AttachWorker` (the caller's handle), `SupLane`, `Held`
- `lane_io.rs`: bounded lane I/O: `LaneError`, `attach_refused_text`, `write_bounded`, `FrameReader`
- `converge.rs`: supervisor-lane connect, probe and `converge_on_ready`; the attach-lane hello and checkpoint collection
- `episode.rs`: the steps of one attach episode that `run_worker` calls in order
- `run.rs`: the worker thread, `run_worker`, and its held-input, retry and link-pause waits
- `quit.rs`: the quit path: `run_end_run_and_wait`, `run_quit`
- `steady.rs`: `QueuedBytes`, the attach reader and the steady-state frame and input handlers
- `support_tests.rs`: test doubles shared by the three test files (`TestClient`, `TestProcess`, `TestEndpoint`)
- `ingress_tests.rs`: tests of bounded ingress, checkpoint deadlines and `QueuedBytes` wakeups
- `converge_tests.rs`: tests of the health probe, absence clock, first attach, link-down pause, dial backoff, attach refusal and abandoned-supervisor spare cleanup
- `steady_tests.rs`: tests of held inputs, take-queue drops, the status-probe keystroke and the held-handshake gate

## Start here
`run.rs`, `run_worker`, for the thread's whole life; then `converge.rs`, `converge_on_ready`, for what precedes the attach.

## Rules
- The worker converges on the supervisor's Ready (`converge_on_ready`) before it dials the attach lane.
- Every lane read and write has a deadline (`write_bounded`, `FrameReader::next_frame`, `HELLO_BUDGET`, `STATUS_BUDGET`, `CHECKPOINT_TRANSFER_BUDGET`).
- Ingress bounds accumulation, not one send: `AttachWorker::send_input` admits an input whenever nothing is reserved, and `IngressReservation` releases its bytes in `Drop` however the message is disposed of.
- An attach refusal reaches the caller with words (`attach_refused_text`), the subscriber cap included.
- A dial whose host link is down waits until the link is up and the client is viewed (`pause_for_link`).
- An unproven supervisor hello or failed Status abandons the endpoint's spare before returning (`connect_supervisor_lane`, `converge_on_ready`, `Endpoint::drop_spare`).
