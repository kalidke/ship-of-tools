//! Carrying out AttachProto's actions for the leg.
use super::*;

// THIS module decides nothing; `attach_proto::AttachProto` does (see
// its module doc). `execute_light_actions` runs the action kinds that
// `output_committed`/`ground_reached`/`checkpoint_ready` can ever
// produce (proven by `attach_proto`'s own implementation: never
// `CommitTake`/`ForwardInput`/`ApplyResize`/`Shutdown`) -- kept SEPARATE
// from the full `execute_actions!` below rather than one macro calling
// itself, because `flush_output` needs to run actions too, and
// `flush_output` is itself called FROM `execute_actions!`'s
// `CommitTake`/`ApplyResize` arms: a macro invoking itself through that
// path is not runtime recursion (which would be fine) but INFINITE
// COMPILE-TIME macro expansion (`recursion limit reached`, hit and
// fixed while building this unit) -- every match arm is expanded
// unconditionally at compile time, `flush_output`'s body included,
// regardless of which arm ever actually runs. Splitting the acyclic
// subset out breaks the cycle: `execute_light_actions` calls nothing
// else here; `flush_output` calls only `execute_light_actions`;
// `execute_actions!` calls `flush_output`, `maybe_rotate`, and (for
// its own light-kind actions) `execute_light_actions` -- all strictly
// "downward", never back.
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
