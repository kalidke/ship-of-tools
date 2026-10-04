//! Carrying out AttachProto's actions for the leg.
use super::*;
use crate::attach_proto::RequestId;

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
                // Round-2 review, finding 7: the Transport contract
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
                // too (Codex round on #194, finding 1). Never
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
                // Codex round-1 Blocker 1 discharge: record the
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
