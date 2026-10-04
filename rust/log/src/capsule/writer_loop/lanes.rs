//! The leg's side of its lanes: transport events in through AttachProto and its actions carried out, in the main loop and through teardown.
use super::output_path::{flush_output, maybe_rotate};
use super::*;
use crate::lane::attach_proto::RequestId;

// THIS module decides nothing; `attach_proto::AttachProto` does (see
// its module doc). `execute_light_actions` runs the action kinds that
// `output_committed`/`ground_reached`/`checkpoint_ready` can ever
// produce (proven by `attach_proto`'s own implementation: never
// `CommitTake`/`ForwardInput`/`ApplyResize`/`Shutdown`) -- kept SEPARATE
// from the full `execute_actions` below: `flush_output` runs actions too
// and is itself called FROM `execute_actions`'s `CommitTake`/`ApplyResize`
// arms. `execute_light_actions` calls nothing else here; `flush_output`
// calls only `execute_light_actions`; `execute_actions` calls
// `flush_output`, `maybe_rotate`, and (for its own light-kind actions)
// `execute_light_actions` -- all strictly "downward", never back.
pub(super) fn execute_light_actions<P: Producer>(seed: Vec<AttachAction>, leg: &mut Leg<'_, P>) {
    let mut queue: VecDeque<AttachAction> = VecDeque::from(seed);
    while let Some(action) = queue.pop_front() {
        match action {
            AttachAction::Send { conn, frame_bytes, marker } => {
                let id = leg.transport.0.send(conn, frame_bytes);
                // the Transport contract
                // (this trait's own doc) requires every outstanding
                // (conn, id) to be unique -- a reused id before its
                // predecessor's completion is reported would
                // silently resolve the WRONG marker for a later
                // `Sent`. A transport that violates this has a bug
                // in it, not something this loop can route around.
                let prior = leg.pending_sends.insert((conn, id), marker);
                assert!(
                    prior.is_none(),
                    "Transport::send returned (conn={conn:?}, id={id}) while a previous send with the \
                     SAME id was still outstanding -- violates the unique-outstanding-id contract"
                );
            }
            AttachAction::Close(conn) => {
                leg.transport.0.close(conn);
                leg.splitters.remove(&conn);
                // Finding 11: purge every pending send this
                // connection still had outstanding -- a canceled
                // write's completion, if the transport ever
                // reported one anyway, must find nothing to apply
                // a marker to.
                leg.pending_sends.retain(|&(c, _), _| c != conn);
                queue.extend(leg.attach_proto.connection_closed(conn, Instant::now()));
            }
            AttachAction::RecordRefusal { conn, reason } => {
                // Diagnostic only -- no wire frame exists for most
                // of these refusals, by design (ADR 0041 decision
                // 5: queue overflow has none at all).
                eprintln!("sot-capsule: attach protocol refusal conn={conn:?} reason={reason:?}");
            }
            AttachAction::BeginCheckpoint { conn } => {
                // A connection that negotiated attach proto v1
                // gets a checkpoint format v1 payload -- no
                // scrollback ring -- regardless of the capsule's
                // own live ring capacity: an old client's own
                // vt100 fork build refuses anything newer
                // outright, and its own pinned
                // `wire::MAX_CHECKPOINT_LEN` predates the ring
                // too. Never
                // silently downgrade a v2 connection; only ever
                // an explicitly negotiated v1 one gets the
                // legacy shape.
                let legacy = leg.attach_proto.negotiated_proto(conn) == wire::ATTACH_PROTO_V1;
                let bytes = if legacy {
                    leg.parser.screen().checkpoint_at_version(LEGACY_CHECKPOINT_VERSION)
                } else {
                    leg.parser.screen().checkpoint()
                }
                .expect(
                    "geometry is bounded to 2x2..512x256, always representable at that range (ADR 0041)",
                );
                queue.extend(leg.attach_proto.checkpoint_ready(conn, bytes, Instant::now()));
            }
            other => unreachable!(
                "execute_light_actions: {other:?} is not one output_committed/ground_reached/checkpoint_ready can produce"
            ),
        }
    }
}


// The full action set -- everything `execute_light_actions` handles,
// delegated one line at a time, PLUS the five action kinds only an
// inbound CLIENT frame can ever produce.
pub(super) fn execute_actions<'t, P: Producer>(seed: Vec<AttachAction>, mut leg: Leg<'t, P>) -> Result<Leg<'t, P>> {
    let mut queue: VecDeque<AttachAction> = VecDeque::from(seed);
    while let Some(action) = queue.pop_front() {
        match action {
            light @ (AttachAction::Send { .. }
            | AttachAction::Close(_)
            | AttachAction::RecordRefusal { .. }
            | AttachAction::BeginCheckpoint { .. }) => {
                execute_light_actions(vec![light], &mut leg);
            }
            AttachAction::CommitTake { conn, controller_id, request_id } => {
                flush_output(&mut leg)?;
                leg.ctx.take_epoch += 1;
                leg.ctx.holder = Some(controller_id.clone());
                let f = leg.ctx.capsule_frame(
                    Class::Lifecycle,
                    json!({"kind": "take_state",
                           "take": {"take_epoch": leg.ctx.take_epoch, "holder": controller_id.clone()}}),
                );
                leg.w.append(&f, Commit::Immediate)?;
                leg.frames_written += 1;
                queue.extend(leg.attach_proto.take_committed(conn, controller_id, leg.ctx.take_epoch, request_id, Instant::now()));
            }
            AttachAction::ForwardInput {
                conn,
                controller_id,
                take_epoch,
                idem_key,
                payload,
                connection_authorized,
                request_id,
            } => {
                let outcome = run_input_wal(
                    &mut leg.ctx,
                    &mut leg.w,
                    &mut leg.store,
                    leg.producer.input(),
                    &mut leg.frames_written,
                    &controller_id,
                    take_epoch,
                    idem_key,
                    &payload,
                    connection_authorized,
                )?;
                leg = maybe_rotate(leg)?;
                queue.extend(leg.attach_proto.input_outcome(conn, outcome, request_id, Instant::now()));
            }
            AttachAction::ApplyResize { conn, cols, rows, request_id } => {
                leg = apply_resize(conn, cols, rows, request_id, &mut queue, leg)?;
            }
            AttachAction::RunEndRequested { reason } => {
                // record the
                // reason HERE, from the marker's own commit -- not
                // only from `Action::Shutdown` (ack-completion-
                // driven), which may never fire at all (a stalled
                // ack, a lost connection). `get_or_insert_with`
                // matches "first commit wins" (step 4): a
                // concurrent second request's reason never
                // overwrites the one that actually got latched.
                leg.shutdown_reason.get_or_insert_with(|| reason.clone());
                commit_run_end_marker(&mut leg.ctx, &mut leg.w, &mut leg.frames_written, &mut leg.run_end_latched, reason)?;
            }
            AttachAction::Shutdown { reason } => {
                leg.shutdown_requested = true;
                leg.shutdown_reason.get_or_insert(reason);
            }
        }
    }
    Ok(leg)
}

fn apply_resize<'t, P: Producer>(
    conn: ConnId,
    cols: u16,
    rows: u16,
    request_id: RequestId,
    queue: &mut VecDeque<AttachAction>,
    mut leg: Leg<'t, P>,
) -> Result<Leg<'t, P>> {
    // ADR 0041: "resize (driver-only) routes into the
    // step-4 exchange unchanged" -- same ordered
    // request -> one ResizePseudoConsole call (skipped
    // if out of budget) -> parser/geometry updated
    // only on success -> outcome shape step 4 already
    // built, now reachable from the wire too.
    flush_output(&mut leg)?;
    let req = leg.ctx.current_controller_frame(
        Class::ControlExchange,
        json!({"phase": "request", "kind_ns": "conpty/resize",
               "to": {"kind": "producer"}, "body": {"cols": cols, "rows": rows}}),
    );
    let req_seq = req.seq;
    leg.w.append(&req, Commit::Immediate)?;
    leg.frames_written += 1;
    let in_budget =
        (MIN_COLS..=MAX_COLS).contains(&cols) && (MIN_ROWS..=MAX_ROWS).contains(&rows);
    let ok = if !in_budget {
        false
    } else {
        leg.resize_os_calls += 1;
        match leg.producer.resize(cols, rows) {
            Ok(()) => {
                leg.parser.screen_mut().set_size(rows, cols);
                true
            }
            Err(_) => false,
        }
    };
    let outcome_body = if ok {
        json!({"disposition": "ok", "cols": cols, "rows": rows})
    } else {
        json!({"disposition": "failed", "cols": cols, "rows": rows,
               "reason": "outside the 2x2..512x256 budget, or ResizePseudoConsole failed"})
    };
    let out = leg.ctx.current_controller_frame(
        Class::ControlExchange,
        json!({"phase": "outcome", "kind_ns": "conpty/resize", "scope": "pty",
               "target": format!("{}:{}", req_seq.epoch, req_seq.n), "body": outcome_body}),
    );
    leg.w.append(&out, Commit::Immediate)?;
    leg.frames_written += 1;
    leg = maybe_rotate(leg)?;
    queue.extend(leg.attach_proto.resize_outcome(conn, ok, cols, rows, request_id, Instant::now()));
    Ok(leg)
}


// Per-pass event quota: a client flood can refill the bounded transport
// channel as fast as this loop drains it, and an UNBOUNDED while-let
// would then starve output commits, tick, and the exit checks
// indefinitely. The quota bounds one pass; the next
// loop iteration resumes immediately, so nothing is dropped -- only
// interleaved.
const TRANSPORT_EVENTS_PER_PASS: usize = 64;

// Drains every currently-available transport event (non-blocking, like
// `commands.try_recv()` below) through `AttachProto`, executing
// whatever it decides. Called every MAIN-LOOP iteration only: once this
// loop is left for teardown, the wire lane's admission is revoked at
// the SAME boundary `commands` already is (`pty` is also moved into the
// Phase-B closer thread by then, so a wire-triggered resize could not
// run even if admitted).
pub(super) fn service_transport_events<'t, P: Producer>(mut leg: Leg<'t, P>) -> Result<Leg<'t, P>> {
    let mut quota = TRANSPORT_EVENTS_PER_PASS;
    while quota > 0 {
        quota -= 1;
        let Some(ev) = leg.transport.0.try_recv_event() else { break };
        match ev {
            TransportEvent::ConnectionOpened(conn) => {
                leg.splitters.insert(conn, wire::FrameSplitter::new());
                leg = execute_actions(leg.attach_proto.connection_opened(conn, Instant::now()), leg)?;
            }
            TransportEvent::Bytes(conn, bytes) => {
                let Some(splitter) = leg.splitters.get_mut(&conn) else { continue };
                let (frames, err) = splitter.feed(&bytes);
                for f in frames {
                    leg = execute_actions(leg.attach_proto.frame(conn, f, Instant::now()), leg)?;
                }
                if err.is_some() {
                    leg.transport.0.close(conn);
                    leg.splitters.remove(&conn);
                    leg.pending_sends.retain(|&(c, _), _| c != conn); // finding 11
                    leg = execute_actions(leg.attach_proto.connection_closed(conn, Instant::now()), leg)?;
                }
            }
            TransportEvent::ConnectionClosed(conn) => {
                leg.splitters.remove(&conn);
                leg.pending_sends.retain(|&(c, _), _| c != conn); // finding 11
                leg = execute_actions(leg.attach_proto.connection_closed(conn, Instant::now()), leg)?;
            }
            TransportEvent::Sent(conn, id) => {
                match leg.pending_sends.remove(&(conn, id)) {
                    Some(marker) => leg = execute_actions(leg.attach_proto.sent(conn, marker, Instant::now()), leg)?,
                    // legitimate ONLY
                    // for a connection this loop already forgot
                    // (closed) -- a late completion racing
                    // the close. For a connection STILL active
                    // (still in `splitters`), an unmatched `Sent`
                    // is a transport contract violation: a
                    // duplicate completion, or one for an id never
                    // actually issued.
                    None => assert!(
                        !leg.splitters.contains_key(&conn),
                        "Transport reported Sent({conn:?}, {id}) for an ACTIVE connection with no \
                         matching outstanding send"
                    ),
                }
            }
            // a terminal transport
            // failure gets the SAME orderly self-end as an
            // externally requested EndRun -- no future connection
            // can ever be admitted, so continuing to run would
            // leave this capsule silently unreachable forever.
            TransportEvent::TransportFatal(detail) => {
                eprintln!(
                    "sot-capsule: transport reported a terminal failure, ending this run: {detail}"
                );
                leg.shutdown_requested = true;
                leg.shutdown_reason = Some("transport-accept-failed".to_string());
            }
        }
    }
    Ok(leg)
}


// Finding 7: producer-bound admission is revoked once EndRun begins
// (`AttachProto::begin_teardown`), but mgmt (`probe`/`status`) and
// `Sent` completions must keep being serviced through BOTH teardown
// phases, until the pipe is explicitly closed -- step 6's adoption
// status-challenge premise depends on it ("revoke admission" applies to
// producer-bound input/resize/take, never to mgmt status/probe). This
// is the teardown-safe action executor: every "light" action
// (Send/Close/RecordRefusal/BeginCheckpoint -- none of which need
// `pty`, already moved into the Phase-B closer thread by the time this
// runs there) delegates to `execute_light_actions`; `RunEndRequested`/
// `Shutdown` (a second EndRun request racing the first) are harmless
// (idempotent past the first marker); `CommitTake`/
// `ForwardInput`/`ApplyResize` are asserted UNREACHABLE --
// `begin_teardown` guarantees `AttachProto` never emits them again at
// the SOURCE, so this is a documented invariant enforced loudly, not a
// live code path (which could not exist here regardless: `pty` is not
// even in scope during Phase B).
pub(super) fn execute_teardown_actions<P: Producer>(seed: Vec<AttachAction>, leg: &mut Leg<'_, P>) -> Result<()> {
    let mut queue: VecDeque<AttachAction> = VecDeque::from(seed);
    while let Some(action) = queue.pop_front() {
        match action {
            light @ (AttachAction::Send { .. }
            | AttachAction::Close(_)
            | AttachAction::RecordRefusal { .. }
            | AttachAction::BeginCheckpoint { .. }) => {
                execute_light_actions(vec![light], leg);
            }
            AttachAction::RunEndRequested { reason } => {
                // A `shutdown` admitted during the final teardown
                // poll (ADR 0041 EndRun step 4's "accepted in the
                // final service poll" case) still latches the SAME
                // way -- mgmt keeps being serviced through both
                // teardown phases, and this is the one
                // place that knows whether the marker already
                // committed. Same reason-recording discipline as
                // the main loop's own arm.
                leg.shutdown_reason.get_or_insert_with(|| reason.clone());
                commit_run_end_marker(&mut leg.ctx, &mut leg.w, &mut leg.frames_written, &mut leg.run_end_latched, reason)?;
            }
            AttachAction::Shutdown { reason } => {
                // `shutdown_requested`
                // is only ever READ inside the main `'main: loop`
                // (the `if shutdown_requested { break 'main ... }`
                // check) -- which has already exited by the time
                // `execute_teardown_actions` ever runs. Setting it
                // here was dead. `shutdown_reason` still matters: a
                // second, teardown-time `Shutdown` (a racing EndRun
                // request) still gets its own reason string folded
                // into `producer_dead`'s eventual detail -- UNLESS
                // an earlier request's reason (via
                // `RunEndRequested`, above) already won (first
                // commit wins, ADR 0041 step 4): `get_or_insert`,
                // not an unconditional overwrite.
                leg.shutdown_reason.get_or_insert(reason);
            }
            other @ (AttachAction::CommitTake { .. }
            | AttachAction::ForwardInput { .. }
            | AttachAction::ApplyResize { .. }) => {
                unreachable!("AttachProto must never emit {other:?} once begin_teardown() has run");
            }
        }
    }
    Ok(())
}


/// As `service_transport_events`, but dispatching through
/// `execute_teardown_actions` -- used by BOTH teardown phases so mgmt
/// traffic and `Sent` completions keep flowing right up until the pipe
/// is closed.
pub(super) fn service_transport_events_teardown<P: Producer>(leg: &mut Leg<'_, P>) -> Result<()> {
    // Same per-pass quota as `service_transport_events`, same reason --
    // teardown's own deadlines must not be defeatable by a client
    // flood refilling the channel mid-drain.
    let mut quota = TRANSPORT_EVENTS_PER_PASS;
    while quota > 0 {
        quota -= 1;
        let Some(ev) = leg.transport.0.try_recv_event() else { break };
        match ev {
            TransportEvent::ConnectionOpened(conn) => {
                leg.splitters.insert(conn, wire::FrameSplitter::new());
                execute_teardown_actions(leg.attach_proto.connection_opened(conn, Instant::now()), leg)?;
            }
            TransportEvent::Bytes(conn, bytes) => {
                let Some(splitter) = leg.splitters.get_mut(&conn) else { continue };
                let (frames, err) = splitter.feed(&bytes);
                for f in frames {
                    execute_teardown_actions(leg.attach_proto.frame(conn, f, Instant::now()), leg)?;
                }
                if err.is_some() {
                    leg.transport.0.close(conn);
                    leg.splitters.remove(&conn);
                    leg.pending_sends.retain(|&(c, _), _| c != conn);
                    execute_teardown_actions(leg.attach_proto.connection_closed(conn, Instant::now()), leg)?;
                }
            }
            TransportEvent::ConnectionClosed(conn) => {
                leg.splitters.remove(&conn);
                leg.pending_sends.retain(|&(c, _), _| c != conn);
                execute_teardown_actions(leg.attach_proto.connection_closed(conn, Instant::now()), leg)?;
            }
            TransportEvent::Sent(conn, id) => {
                match leg.pending_sends.remove(&(conn, id)) {
                    Some(marker) => execute_teardown_actions(leg.attach_proto.sent(conn, marker, Instant::now()), leg)?,
                    // Finding 7, same reasoning as the main loop's
                    // identical arm: tolerated only for a
                    // connection already closed.
                    None => assert!(
                        !leg.splitters.contains_key(&conn),
                        "Transport reported Sent({conn:?}, {id}) for an ACTIVE connection with no \
                         matching outstanding send"
                    ),
                }
            }
            TransportEvent::TransportFatal(detail) => {
                // teardown-phase
                // analog of `AttachAction::Shutdown`'s own
                // teardown-time arm just above: `shutdown_requested`
                // is dead here (already left `'main`), but a fatal
                // transport failure arriving DURING teardown still
                // deserves its own reason folded into the eventual
                // `producer_dead` detail -- unless a real reason is
                // already recorded (the run is ending for some
                // OTHER cause; don't overwrite it with a fatal
                // event that is likely just this SAME pipe closing
                // as a side effect of that other teardown).
                eprintln!(
                    "sot-capsule: transport reported a terminal failure during teardown: {detail}"
                );
                leg.shutdown_reason.get_or_insert_with(|| "transport-accept-failed".to_string());
            }
        }
    }
    Ok(())
}


// the ack-grace window's own
// drain -- STOP ADMITTING new connections or new request bytes once
// the final ordinary teardown poll is behind us, so a request that
// slips in with, say, 50ms left in the grace can never be credited
// with the full 2s the "final service poll" guarantee actually
// promises. `Sent`/`ConnectionClosed` still drain normally (the whole
// POINT of the grace is letting an ALREADY-QUEUED ack finish); a brand
// new `ConnectionOpened` or a new `Bytes` payload on an existing
// connection is closed outright, WITHOUT ever reaching `attach_proto`
// -- no admission, so no new obligation this bounded window cannot
// keep.
pub(super) fn drain_pending_sends_only<P: Producer>(leg: &mut Leg<'_, P>) -> Result<()> {
    let mut quota = TRANSPORT_EVENTS_PER_PASS;
    while quota > 0 {
        quota -= 1;
        let Some(ev) = leg.transport.0.try_recv_event() else { break };
        match ev {
            TransportEvent::ConnectionOpened(conn) => {
                // Never admitted: no splitter, no `attach_proto`
                // event, just closed.
                leg.transport.0.close(conn);
            }
            TransportEvent::Bytes(conn, _bytes) => {
                // A connection admitted during ORDINARY teardown
                // (before the grace began) sending more bytes now:
                // still no new admission -- close it, purging
                // whatever this loop already tracked for it.
                leg.transport.0.close(conn);
                leg.splitters.remove(&conn);
                leg.pending_sends.retain(|&(c, _), _| c != conn);
                execute_teardown_actions(leg.attach_proto.connection_closed(conn, Instant::now()), leg)?;
            }
            TransportEvent::ConnectionClosed(conn) => {
                leg.splitters.remove(&conn);
                leg.pending_sends.retain(|&(c, _), _| c != conn);
                execute_teardown_actions(leg.attach_proto.connection_closed(conn, Instant::now()), leg)?;
            }
            TransportEvent::Sent(conn, id) => {
                match leg.pending_sends.remove(&(conn, id)) {
                    Some(marker) => execute_teardown_actions(leg.attach_proto.sent(conn, marker, Instant::now()), leg)?,
                    None => assert!(
                        !leg.splitters.contains_key(&conn),
                        "Transport reported Sent({conn:?}, {id}) for an ACTIVE connection with no \
                         matching outstanding send"
                    ),
                }
            }
            TransportEvent::TransportFatal(detail) => {
                eprintln!(
                    "sot-capsule: transport reported a terminal failure during the shutdown-ack grace: {detail}"
                );
                leg.shutdown_reason.get_or_insert_with(|| "transport-accept-failed".to_string());
            }
        }
    }
    Ok(())
}
