//! `run`: the capsule writer loop, one producer from spawn to sealed voyage.
use super::*;
use std::ops::ControlFlow;

mod lanes;
mod start;
use lanes::execute_light_actions;
use start::start;

struct ShutdownGuard<'a>(&'a mut dyn Transport);
impl Drop for ShutdownGuard<'_> {
    fn drop(&mut self) {
        // This Drop is the FALLBACK path only (an early `?` return
        // before the designed, explicit call at real teardown time
        // ever runs, or that call's own redundant second pass) -- it
        // has no outer aggregate deadline to share, so it computes a
        // fresh one. `run` has ALREADY returned by the time this
        // executes (it is dropping `run`'s own locals during
        // unwind/return), so a `false` here can only be reported
        // loudly (stderr), never turned into `run`'s result.
        if !self.0.shutdown_all(Instant::now() + TEARDOWN_AGGREGATE_DEADLINE) {
            eprintln!(
                "sot-capsule: ShutdownGuard's fallback teardown did not complete within its \
                 aggregate deadline; a worker thread may still be running"
            );
        }
    }
}

// Per-pass event quota: a client flood can refill the bounded transport
// channel as fast as this loop drains it, and an UNBOUNDED while-let
// would then starve output commits, tick, and the exit checks
// indefinitely (review finding). The quota bounds one pass; the next
// loop iteration resumes immediately, so nothing is dropped -- only
// interleaved.
const TRANSPORT_EVENTS_PER_PASS: usize = 64;

/// The leg's writer-loop state from `start` to `seal_run`: every value two or more of
/// `run`'s pieces share. Each piece takes it last: by `&mut`, or by value where the piece may
/// rotate or seal the segment (`SegmentWriter::seal` consumes the writer).
///
/// FIELD ORDER IS LOAD-BEARING for the last six fields. Fields drop in declaration order, and
/// these six drop in the order `run`'s locals did before it was cut: the output budget is
/// cancelled, the producer's domain dropped, the segment file closed, the output channel
/// closed, the transport shut down, and the writer lock released last. No other field's drop
/// does anything. Never implement `Drop` for this struct.
struct Leg<'t, P> {
    /// Frame factory: sequence numbers, clocks, the durable take epoch and holder.
    ctx: FrameCtx,
    /// The features every segment of this run declares; rotation reuses them.
    segment_features: Vec<String>,
    /// Estimated bytes in the open segment; rotation starts at `SEGMENT_MAX_BYTES`.
    seg_bytes: u64,
    /// Frames appended this run (`ExitSummary`).
    frames_written: u64,
    /// Segments sealed this run (`ExitSummary`).
    segments_sealed: u64,
    /// The attach protocol: it decides, the loop carries out its actions.
    attach_proto: AttachProto,
    /// Frame reassembly per open connection; a connection is active while it has one.
    splitters: HashMap<ConnId, wire::FrameSplitter>,
    /// Sends not yet reported written, by (connection, send id).
    pending_sends: HashMap<(ConnId, u64), Option<SentMarker>>,
    /// Ends the main loop at its next check (shutdown ack written, or transport failed).
    shutdown_requested: bool,
    /// The reason `producer_dead` records; the first one set wins.
    shutdown_reason: Option<String>,
    /// The run-end marker is committed; never unset.
    run_end_latched: bool,
    /// Coalesces transport wakes; a loop clears it when it consumes one.
    wake_pending: Arc<AtomicBool>,
    /// The live terminal screen, for checkpoints and the ground gate.
    parser: vt100_ctt::Parser,
    /// Finds the host DA1 query in producer output.
    handshake: HostHandshake,
    /// The first DA1 query was answered and recorded (`ExitSummary`).
    dsr_answered: bool,
    /// DA1 queries after the first, counted only (`ExitSummary`).
    handshake_suppressed_matches: u64,
    /// Resize calls that reached the producer (`ExitSummary`).
    resize_os_calls: u64,
    /// An output end before teardown that the producer's exit explained; `producer_dead` records it.
    output_ended_early: Option<String>,
    /// The reader's byte budget; the loop releases what it records.
    output_budget: Arc<OutputBudget>,
    /// Output recorded since the last commit; published only after that commit's fsync.
    pending_output: Vec<u8>,
    /// Bytes in `pending_output`; the group-commit trigger.
    pending_bytes: usize,
    /// The last commit (group-commit window).
    last_commit: Instant,
    /// The last fsync (minimum gap between idle commits).
    last_fsync: Instant,
    /// The last output (idle commit).
    last_output: Instant,
    /// Output handled since the pacer last slept.
    bytes_since_yield: usize,
    /// Cancels the budget on any exit, so a reader blocked in `reserve` wakes. Drops first.
    _budget_guard: BudgetCancelGuard,
    /// The producer and its kill domain.
    producer: P,
    /// The open segment.
    w: SegmentWriter,
    /// Producer output, the reader's end, and transport wakes: one channel.
    output_rx: mpsc::Receiver<ReaderEvent>,
    /// Shuts the transport down on any exit, while the writer lock is still held.
    transport: ShutdownGuard<'t>,
    /// The voyage store; it holds the writer lock. Drops last.
    store: VoyageStore,
}

/// Run one producer under a capsule, generic over `P: Producer` (ADR 0043
/// "Decisions for LU2"). Blocks until the run ends — either the producer
/// exits on its own, `commands` delivers [`Command::Kill`], or the mgmt
/// lane's `shutdown` drives `Action::Shutdown` once its ack is physically
/// written (ADR 0041 Lifecycle: an externally requested end — EndRun).
/// `commands` mirrors `claude.rs`'s `operator: mpsc::Receiver<OperatorCmd>`
/// parameter; the caller owns its `Sender` (see the module doc's
/// stdin-ownership point). `transport` is the ADR 0041 step-5 pipe
/// protocol's seam: a real named pipe on Windows (U3) or a test transport
/// (this unit) drives the SAME [`AttachProto`] state machine either way,
/// polled every iteration via [`Transport::try_recv_event`] — see
/// `attach_proto`'s module doc for the protocol this loop executes.
pub fn run<P: Producer>(
    config: CapsuleConfig,
    commands: mpsc::Receiver<Command>,
    transport: &mut dyn Transport,
) -> Result<ExitSummary> {
    let (mut leg, reader_handle, spawned_at) = match start::<P>(&config, transport)? {
        ControlFlow::Continue(running) => running,
        ControlFlow::Break(summary) => return Ok(summary),
    };

    macro_rules! flush_output {
        ($w:expr) => {
            if leg.pending_bytes > 0 {
                $w.commit()?; // the watermark: fsync BEFORE anything is published
                leg.last_fsync = Instant::now();
                execute_light_actions(leg.attach_proto.output_committed(&leg.pending_output, Instant::now()), &mut leg);
                leg.pending_output.clear();
                leg.pending_bytes = 0;
            } else if $w.has_unsynced() {
                // A Buffered input-WAL record with no output behind it: commit it
                // by the next group-commit check (ADR 0039 Durability invariants);
                // nothing is published, so no `output_committed`.
                $w.commit()?;
                leg.last_fsync = Instant::now();
            }
            leg.last_commit = Instant::now();
            // ADR 0041: attach is GROUND-GATED; the watermark barrier
            // (force pending commit -> publish to EXISTING subscribers ->
            // checkpoint -> subscribe) is exactly this ordering -- publish
            // above already ran, so a ground boundary found HERE is the
            // single loop step the barrier requires.
            if leg.parser.is_ground() {
                execute_light_actions(leg.attach_proto.ground_reached(Instant::now()), &mut leg);
            }
        };
    }

    macro_rules! maybe_rotate {
        ($w:expr) => {
            if leg.seg_bytes >= SEGMENT_MAX_BYTES {
                flush_output!($w);
                let digest = $w.seal(None)?;
                leg.store.advance_chain(digest);
                leg.segments_sealed += 1;
                $w = leg.store.open_segment_with_features(wall_ms(), leg.segment_features.clone())?;
                leg.seg_bytes = 0;
            }
        };
    }

    /// Bounds consecutive output work to one `GROUP_COMMIT_BYTES` worth
    /// (the SAME threshold the writer already paces its own fsyncs by)
    /// before yielding this thread -- the loop-fairness fix above. Takes
    /// `bytes` itself (not a pre-computed length) so every call site reads
    /// as "handle this chunk, paced" in one line: `.len()` borrows before
    /// `handle_output!` moves it.
    /// Offers a scheduling window to another ready thread every
    /// `GROUP_COMMIT_BYTES` worth of output -- a timed sleep (see
    /// the doc above `bytes_since_yield`), not a bound: it may do nothing.
    macro_rules! pace_output {
        ($bytes:ident) => {
            leg.bytes_since_yield += $bytes.len();
            handle_output!($bytes);
            if leg.bytes_since_yield >= GROUP_COMMIT_BYTES {
                std::thread::sleep(Duration::from_millis(1));
                leg.bytes_since_yield = 0;
            }
        };
    }

    /// Attaching to an idle session (real CI failure, windows-latest
    /// only): `ground_reached` was previously fed ONLY from
    /// `flush_output!`, itself reached only by fresh output crossing the
    /// group-commit threshold, or a periodic idle check gated behind the
    /// OUTPUT CHANNEL's own `recv_timeout` cadence — never directly by
    /// admission, and never by `tick`, the one hook this loop already
    /// calls unconditionally every iteration. An attach landing on an
    /// ALREADY-idle, already-at-ground session (a shell sitting at its
    /// prompt — the ordinary case, exercised once the fidelity test's
    /// producer goes silent after `--linger`) depended entirely on that
    /// separate cadence happening to notice, which is exactly the kind of
    /// dependency `pace_output!`'s own history above already proved
    /// fragile on a loaded windows-latest runner: the attach pended for
    /// the full 5 s `GroundTimeout` and was refused instead of completing
    /// on the very next iteration.
    ///
    /// Called every iteration, right after `tick`, so it runs in the SAME
    /// iteration an attach was just admitted in (a) and on every
    /// subsequent iteration while one still pends (b) — no separate
    /// cadence to depend on. Scoped behind `ground_gate_pending()` (a
    /// cheap check) so the vastly more common "nothing pending" iteration
    /// pays nothing beyond it. Watermark semantics stay exact: with no
    /// pending uncommitted bytes, NOW already is a valid commit boundary
    /// (`flush_output!` skips the commit but still evaluates ground); with
    /// some pending, `flush_output!` forces the SAME commit-then-check
    /// barrier it always runs, just immediately rather than waiting for
    /// the group-commit threshold or the idle timer to get to it.
    macro_rules! eager_ground_check {
        () => {
            if leg.attach_proto.ground_gate_pending() {
                flush_output!(leg.w);
            }
        };
    }

    // The full action set -- everything `execute_light_actions` handles,
    // delegated one line at a time (never re-expanding `flush_output!`
    // itself), PLUS the five action kinds only an inbound CLIENT frame can
    // ever produce.
    macro_rules! execute_actions {
        ($seed:expr) => {{
            let mut queue: VecDeque<AttachAction> = VecDeque::from($seed);
            while let Some(action) = queue.pop_front() {
                match action {
                    light @ (AttachAction::Send { .. }
                    | AttachAction::Close(_)
                    | AttachAction::RecordRefusal { .. }
                    | AttachAction::BeginCheckpoint { .. }) => {
                        execute_light_actions(vec![light], &mut leg);
                    }
                    AttachAction::CommitTake { conn, controller_id, request_id } => {
                        flush_output!(leg.w);
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
                        maybe_rotate!(leg.w);
                        queue.extend(leg.attach_proto.input_outcome(conn, outcome, request_id, Instant::now()));
                    }
                    AttachAction::ApplyResize { conn, cols, rows, request_id } => {
                        // ADR 0041: "resize (driver-only) routes into the
                        // step-4 exchange unchanged" -- same ordered
                        // request -> one ResizePseudoConsole call (skipped
                        // if out of budget) -> parser/geometry updated
                        // only on success -> outcome shape step 4 already
                        // built, now reachable from the wire too.
                        flush_output!(leg.w);
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
                        maybe_rotate!(leg.w);
                        queue.extend(leg.attach_proto.resize_outcome(conn, ok, cols, rows, request_id, Instant::now()));
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
        }};
    }

    // Drains every currently-available transport event (non-blocking, like
    // `commands.try_recv()` below) through `AttachProto`, executing
    // whatever it decides. Called every MAIN-LOOP iteration only: once this
    // loop is left for teardown, the wire lane's admission is revoked at
    // the SAME boundary `commands` already is (`pty` is also moved into the
    // Phase-B closer thread by then, so a wire-triggered resize could not
    // run even if admitted).
    macro_rules! service_transport_events {
        () => {
            let mut quota = TRANSPORT_EVENTS_PER_PASS;
            while quota > 0 {
                quota -= 1;
                let Some(ev) = leg.transport.0.try_recv_event() else { break };
                match ev {
                    TransportEvent::ConnectionOpened(conn) => {
                        leg.splitters.insert(conn, wire::FrameSplitter::new());
                        execute_actions!(leg.attach_proto.connection_opened(conn, Instant::now()));
                    }
                    TransportEvent::Bytes(conn, bytes) => {
                        let Some(splitter) = leg.splitters.get_mut(&conn) else { continue };
                        let (frames, err) = splitter.feed(&bytes);
                        for f in frames {
                            execute_actions!(leg.attach_proto.frame(conn, f, Instant::now()));
                        }
                        if err.is_some() {
                            leg.transport.0.close(conn);
                            leg.splitters.remove(&conn);
                            leg.pending_sends.retain(|&(c, _), _| c != conn); // finding 11
                            execute_actions!(leg.attach_proto.connection_closed(conn, Instant::now()));
                        }
                    }
                    TransportEvent::ConnectionClosed(conn) => {
                        leg.splitters.remove(&conn);
                        leg.pending_sends.retain(|&(c, _), _| c != conn); // finding 11
                        execute_actions!(leg.attach_proto.connection_closed(conn, Instant::now()));
                    }
                    TransportEvent::Sent(conn, id) => {
                        match leg.pending_sends.remove(&(conn, id)) {
                            Some(marker) => execute_actions!(leg.attach_proto.sent(conn, marker, Instant::now())),
                            // Round-2 review, finding 7: legitimate ONLY
                            // for a connection this loop already forgot
                            // (closed, `pending_sends` purged by finding
                            // 11's own retain) -- a late completion racing
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
                    // Round-2 e2e review, finding 4: a terminal transport
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
        };
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
    macro_rules! execute_teardown_actions {
        ($seed:expr) => {{
            let mut queue: VecDeque<AttachAction> = VecDeque::from($seed);
            while let Some(action) = queue.pop_front() {
                match action {
                    light @ (AttachAction::Send { .. }
                    | AttachAction::Close(_)
                    | AttachAction::RecordRefusal { .. }
                    | AttachAction::BeginCheckpoint { .. }) => {
                        execute_light_actions(vec![light], &mut leg);
                    }
                    AttachAction::RunEndRequested { reason } => {
                        // A `shutdown` admitted during the final teardown
                        // poll (ADR 0041 EndRun step 4's "accepted in the
                        // final service poll" case) still latches the SAME
                        // way -- mgmt keeps being serviced through both
                        // teardown phases (finding 7), and this is the one
                        // place that knows whether the marker already
                        // committed. Same reason-recording discipline as
                        // the main loop's own arm (Codex round-1 Blocker 1).
                        leg.shutdown_reason.get_or_insert_with(|| reason.clone());
                        commit_run_end_marker(&mut leg.ctx, &mut leg.w, &mut leg.frames_written, &mut leg.run_end_latched, reason)?;
                    }
                    AttachAction::Shutdown { reason } => {
                        // Round-2 review deletion residue: `shutdown_requested`
                        // is only ever READ inside the main `'main: loop`
                        // (the `if shutdown_requested { break 'main ... }`
                        // check) -- which has already exited by the time
                        // `execute_teardown_actions!` ever runs. Setting it
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
        }};
    }

    /// As `service_transport_events!`, but dispatching through
    /// `execute_teardown_actions!` -- used by BOTH teardown phases so mgmt
    /// traffic and `Sent` completions keep flowing right up until the pipe
    /// is closed (finding 7).
    macro_rules! service_transport_events_teardown {
        () => {
            // Same per-pass quota as the main loop's macro, same reason --
            // teardown's own deadlines must not be defeatable by a client
            // flood refilling the channel mid-drain (review finding).
            let mut quota = TRANSPORT_EVENTS_PER_PASS;
            while quota > 0 {
                quota -= 1;
                let Some(ev) = leg.transport.0.try_recv_event() else { break };
                match ev {
                    TransportEvent::ConnectionOpened(conn) => {
                        leg.splitters.insert(conn, wire::FrameSplitter::new());
                        execute_teardown_actions!(leg.attach_proto.connection_opened(conn, Instant::now()));
                    }
                    TransportEvent::Bytes(conn, bytes) => {
                        let Some(splitter) = leg.splitters.get_mut(&conn) else { continue };
                        let (frames, err) = splitter.feed(&bytes);
                        for f in frames {
                            execute_teardown_actions!(leg.attach_proto.frame(conn, f, Instant::now()));
                        }
                        if err.is_some() {
                            leg.transport.0.close(conn);
                            leg.splitters.remove(&conn);
                            leg.pending_sends.retain(|&(c, _), _| c != conn);
                            execute_teardown_actions!(leg.attach_proto.connection_closed(conn, Instant::now()));
                        }
                    }
                    TransportEvent::ConnectionClosed(conn) => {
                        leg.splitters.remove(&conn);
                        leg.pending_sends.retain(|&(c, _), _| c != conn);
                        execute_teardown_actions!(leg.attach_proto.connection_closed(conn, Instant::now()));
                    }
                    TransportEvent::Sent(conn, id) => {
                        match leg.pending_sends.remove(&(conn, id)) {
                            Some(marker) => execute_teardown_actions!(leg.attach_proto.sent(conn, marker, Instant::now())),
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
                        // Round-2 e2e review, finding 4, teardown-phase
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
        };
    }

    // U1a Codex round-1, Major 6 discharge: the ack-grace window's own
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
    macro_rules! drain_pending_sends_only {
        () => {
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
                        execute_teardown_actions!(leg.attach_proto.connection_closed(conn, Instant::now()));
                    }
                    TransportEvent::ConnectionClosed(conn) => {
                        leg.splitters.remove(&conn);
                        leg.pending_sends.retain(|&(c, _), _| c != conn);
                        execute_teardown_actions!(leg.attach_proto.connection_closed(conn, Instant::now()));
                    }
                    TransportEvent::Sent(conn, id) => {
                        match leg.pending_sends.remove(&(conn, id)) {
                            Some(marker) => execute_teardown_actions!(leg.attach_proto.sent(conn, marker, Instant::now())),
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
        };
    }

    // One producer-output handler, used identically pre-teardown AND
    // during BOTH teardown phases — so "the handshake keeps answering
    // through the drain" (ADR 0041) can't be missed by one call site and
    // not the other. Feeds the live parser, answers the FIRST DA1 query
    // ever seen (recording request -> response -> outcome), records the
    // raw producer frame, and tracks the group-commit/echo state.
    macro_rules! handle_output {
        ($bytes:expr) => {{
            let bytes = $bytes;
            leg.parser.process(&bytes);

            use base64_engine::encode_b64;
            let f = leg.ctx.producer_frame(json!({"bytes_b64": encode_b64(&bytes)}));
            leg.w.append(&f, Commit::Buffered)?;
            leg.frames_written += 1;
            leg.seg_bytes += bytes.len() as u64 + 128;
            leg.output_budget.release(bytes.len() as u64);
            leg.pending_output.extend_from_slice(&bytes);
            leg.pending_bytes += bytes.len();
            if leg.pending_bytes >= GROUP_COMMIT_BYTES {
                flush_output!(leg.w);
            }

            let matches = leg.handshake.feed(&bytes);
            if matches > 0 {
                if !leg.dsr_answered {
                    leg.dsr_answered = true;
                    // Query exchange, ADR 0041's own phrase and shape:
                    // request -> response (only on a successful write) ->
                    // outcome (always, reflecting whether it was).
                    let req = leg.ctx.capsule_frame(
                        Class::ControlExchange,
                        json!({"phase": "request", "kind_ns": "conpty/host-handshake",
                               "to": {"kind": "producer"}, "body": {"query": "da1"}}),
                    );
                    let req_seq = req.seq;
                    leg.w.append(&req, Commit::Immediate)?;
                    leg.frames_written += 1;

                    let write_result = leg.producer.input().write_all(host_handshake::DA1_REPLY);
                    if write_result.is_ok() {
                        let mut resp = leg.ctx.capsule_frame(
                            Class::ControlExchange,
                            json!({"phase": "response", "kind_ns": "conpty/host-handshake",
                                   "body": {"query": "da1"}}),
                        );
                        resp.refs = vec![FrameRef { kind: RefKind::RespondsTo, frame: req_seq }];
                        leg.w.append(&resp, Commit::Immediate)?;
                        leg.frames_written += 1;
                    }
                    let outcome_body = match &write_result {
                        Ok(()) => json!({"disposition": "ok"}),
                        Err(e) => json!({"disposition": "failed", "reason": e.to_string()}),
                    };
                    let out = leg.ctx.capsule_frame(
                        Class::ControlExchange,
                        json!({"phase": "outcome", "kind_ns": "conpty/host-handshake", "scope": "pty",
                               "target": format!("{}:{}", req_seq.epoch, req_seq.n), "body": outcome_body}),
                    );
                    leg.w.append(&out, Commit::Immediate)?;
                    leg.frames_written += 1;

                    // Any FURTHER matches in this SAME chunk are already
                    // "later" than the one just answered.
                    leg.handshake_suppressed_matches += (matches - 1) as u64;
                } else {
                    leg.handshake_suppressed_matches += matches as u64;
                }
            }
        }};
    }

    // Main loop: natural-exit polled every iteration (bounded to one
    // GROUP_COMMIT_WINDOW of latency, regardless of event volume); the
    // caller's command channel is polled NON-BLOCKINGLY (rare traffic, and
    // this is the last point it is EVER polled — teardown never touches it
    // again, which is what makes admission revocation real). The wire
    // transport is serviced every iteration too (`service_transport_events!`
    // + `tick`) — see the module doc's "Step 5 (U2)" section.

    let exit_kind = 'main: loop {
        if leg.producer.wait(Duration::ZERO)? {
            break 'main ExitKind::ProducerExited;
        }
        service_transport_events!();
        execute_actions!(leg.attach_proto.tick(Instant::now()));
        eager_ground_check!();
        // ADR 0041 EndRun step 2 / Codex round-1 Blocker 1 discharge: the
        // LATCH drives teardown, not the ack -- "ack completion only
        // ACCELERATES teardown". `shutdown_requested` alone (the OLD,
        // ack-completion-only trigger via `AttachAction::Shutdown`, and the
        // transport-fatal self-end path) is not enough: a stalled ack, a
        // client that stops reading, a progress-deadline close, or a lost
        // connection must still tear this run down once the marker is
        // durable, exactly the cases ADR 0041 lists as unable to unlatch
        // it. The ack remains a courtesy -- serviced normally through
        // teardown (still tracked via `pending_sends`/the ack-grace window)
        // but never a precondition for STARTING it.
        if leg.shutdown_requested || leg.run_end_latched {
            break 'main ExitKind::Requested;
        }
        match commands.try_recv() {
            // Major 6 discharge: `Command::Kill` is the direct-caller/
            // supervisor own-behalf EndRun primitive (this module's own
            // doc on `Command`) -- it must carry a reason and route
            // through the SAME commit/latch transition as a wire
            // `shutdown`, or a resume could respawn a run that a caller
            // deliberately ended. Idempotent like every other caller of
            // `commit_run_end_marker`: a concurrent wire shutdown racing
            // this Kill still writes only one marker.
            Ok(Command::Kill) => {
                leg.shutdown_reason.get_or_insert_with(|| "operator_kill".to_string());
                commit_run_end_marker(
                    &mut leg.ctx,
                    &mut leg.w,
                    &mut leg.frames_written,
                    &mut leg.run_end_latched,
                    "operator_kill".to_string(),
                )?;
                break 'main ExitKind::Requested;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            // The caller dropped its `Sender` — NOT a kill (ADR: no
            // channel-disconnect-as-kill, "no exit code, no FE event, no
            // supervisor inference may request one"). Just means no
            // FUTURE commands will arrive; keep running on natural-exit
            // polling alone. `try_recv` on an already-disconnected channel
            // returns immediately, so there is no cost to leaving this
            // arm empty rather than tracking "stop trying".
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
        // Switch-latency Phase 1 (c): a `Transport` event arriving DURING
        // this wait no longer waits out the full window before this loop
        // notices — `transport.0.set_wake`'s callback (registered above,
        // before `bind`) pushes `ReaderEvent::TransportActivity` on the
        // SAME channel this `recv_timeout` already blocks on, the instant
        // the transport queues a fresh event (AFTER queuing it — see that
        // callback's own doc — so `service_transport_events!` at the top
        // of the NEXT iteration is guaranteed to find it). `Transport::
        // try_recv_event` itself is still never blocking (its own
        // contract, unchanged); this wait is what wakes early, not that
        // drain. `GROUP_COMMIT_WINDOW` stays the bound on how long output
        // may batch under sustained load and the cadence when nothing is
        // pending; with output pending, `OUTPUT_IDLE` of quiet commits it.
        match leg.output_rx.recv_timeout(output_wait(
            leg.pending_bytes,
            leg.last_commit.elapsed(),
            leg.last_output.elapsed(),
            leg.last_fsync.elapsed(),
        )) {
            Ok(ReaderEvent::Output(bytes)) => {
                leg.last_output = Instant::now();
                pace_output!(bytes);
                maybe_rotate!(leg.w);
            }
            Ok(ReaderEvent::TransportActivity) => {
                leg.wake_pending.store(false, Ordering::Release);
                // Nothing else to do: `service_transport_events!` at this
                // loop's own top (next iteration) drains and processes
                // whatever prompted this wake.
            }
            Ok(ReaderEvent::Done(result)) => {
                // The output side ended before this loop closed it, whether
                // by a graceful EOF or a real error. Fatal UNLESS the
                // producer's own exit explains it (ADR 0043 decision 12, as
                // amended): on macOS the session leader's exit revokes every
                // fd on the pty, this loop's deliberately held slave
                // included, so the reader's terminal state can PRECEDE the
                // exit being observable. Confirm it, bounded; never assume
                // it. Unconfirmed, this stays the anomaly it always was --
                // ConPTY keeps `hOutput` open regardless of child lifetime
                // until explicitly closed, and Linux's held slave means the
                // master sees nothing before the loop drops it, so on those
                // platforms only a capsule-runtime defect gets here -- and it
                // bails unsealed, matching ADR 0039's crash shape: recovery
                // seals whatever valid prefix already committed.
                if !leg.producer.wait(READER_END_EXIT_GRACE)? {
                    return Err(Error::State(format!(
                        "capsule_win: reader reached its terminal state before close_pty was ever \
                         called, and the producer was still alive {READER_END_EXIT_GRACE:?} later: {result:?}"
                    )));
                }
                leg.output_ended_early = Some(format!("{result:?}"));
                break 'main ExitKind::ProducerExited;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(ReaderEvent::ReaderGone) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Codex review (PR #227): `ReaderGone` (an explicit send,
                // including from a panic unwind — see its own doc) and a
                // bare channel disconnect are the SAME condition now — the
                // reader thread is gone without having sent a terminal
                // `Done` — so they share this one arm rather than treating
                // an unwind as a distinct, undiagnosed case.
                return Err(Error::State(
                    "capsule_win: the reader thread ended without a terminal Done event".into(),
                ));
            }
        }
        // Codex review (PR #227): checked here, after the match rather
        // than only inside its `Timeout` arm, so a transport wake (or any
        // other non-`Timeout` result) can never starve this deadline —
        // continuous transport activity every `recv_timeout` call used to
        // mean `last_commit.elapsed()` was never even read.
        if should_flush_output(leg.last_commit.elapsed(), leg.pending_bytes, leg.last_output.elapsed(), leg.last_fsync.elapsed()) {
            flush_output!(leg.w);
        }
    };
    // N1 (Codex review round 3, owner-corrected): captured HERE, the
    // instant the main loop concludes for EITHER exit kind -- NOT after
    // the teardown machinery below (job reap, ConPTY drain, the
    // aggregate deadline, a final wait), which alone can outlast the
    // producer's own life and would otherwise pollute this measurement
    // with exactly the capsule-side latency the supervisor's own
    // anti-flap counter must never see (an earlier version of this fix
    // measured it at the LATE producer_dead-detail-construction site
    // below, reproducing the identical bug it exists to close, just
    // moved inside this process instead of the supervisor's). For
    // ProducerExited the producer is already dead by definition; for
    // Requested it is about to be forcibly killed by
    // `producer.terminate_domain()` a few lines into teardown, with no
    // intervening I/O between here and there.
    let producer_uptime_ms = u64::try_from(spawned_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    flush_output!(leg.w);

    // Producer-bound admission (take/input/resize) is revoked from here on
    // (finding 7) — but mgmt (probe/status/shutdown) and Sent completions
    // keep being serviced through BOTH phases below, via
    // `service_transport_events_teardown!`, until the pipe closes (see that
    // macro's own doc for why, and `execute_teardown_actions!` for the
    // reduced action set this implies).
    leg.attach_proto.begin_teardown();

    // ONE teardown orchestrator (ADR 0041: "Teardown has ONE orchestrator")
    // for both exit_kind::ProducerExited and exit_kind::Requested — every
    // step below is unconditional: terminating an already-empty job is a
    // harmless no-op. `commands` is never read again from this point on —
    // real admission revocation (module doc), not receive-then-discard.
    //
    // Phase A: terminate the job, then REAP-POLL `ActiveProcesses` WHILE
    // STILL SERVICING `output_rx` (committing frames, answering the
    // handshake) AND the transport (mgmt/Sent, per finding 7) — review
    // finding, the blocker: the previous version polled the job with
    // nobody draining the channel, so a reader already blocked in
    // `OutputBudget::reserve` (or a DA1 only this loop could answer) could
    // leave `hOutput` undrained right when `ClosePseudoConsole` needed it
    // drained, and Microsoft's own docs say a pre-24H2 build's close can
    // wait indefinitely under exactly that condition.
    leg.producer.terminate_domain()?;
    let reap_deadline = Instant::now() + TEARDOWN_REAP_TIMEOUT;
    loop {
        service_transport_events_teardown!();
        execute_teardown_actions!(leg.attach_proto.tick(Instant::now()));
        eager_ground_check!();
        if leg.producer.domain_is_empty()? {
            break;
        }
        if Instant::now() >= reap_deadline {
            return Err(Error::State(
                "capsule_win: job did not reap within the teardown timeout".into(),
            ));
        }
        match leg.output_rx.recv_timeout(TEARDOWN_REAP_POLL) {
            Ok(ReaderEvent::Output(bytes)) => {
                pace_output!(bytes);
                maybe_rotate!(leg.w);
            }
            Ok(ReaderEvent::TransportActivity) => {
                // Switch-latency Phase 1 (c): same wake, same channel, as
                // the main loop's own arm -- mgmt/`Sent` traffic keeps
                // being serviced through teardown (finding 7), so it gets
                // the same early wake here rather than waiting out
                // `TEARDOWN_REAP_POLL`. `service_transport_events_teardown!`
                // at this loop's own top does the actual draining.
                leg.wake_pending.store(false, Ordering::Release);
            }
            Ok(ReaderEvent::Done(result)) => {
                // The same rule as the main loop's identical arm, and for
                // the same reason: nothing has called close_pty() yet, so
                // this cannot be an ordinary end of the drain -- it is the
                // producer's own exit revoking the pty, or it is a defect.
                // The difference here is only that this loop has a reap to
                // finish, so it records and keeps polling.
                if !leg.producer.wait(READER_END_EXIT_GRACE)? {
                    return Err(Error::State(format!(
                        "capsule_win: reader reached its terminal state during reap with the producer \
                         still alive {READER_END_EXIT_GRACE:?} later: {result:?}"
                    )));
                }
                leg.output_ended_early = Some(format!("{result:?}"));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {} // just recheck active_processes
            Ok(ReaderEvent::ReaderGone) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Codex review (PR #227): see the main loop's identical arm
                // — `ReaderGone` and a bare disconnect are the same "reader
                // thread is gone without a terminal Done" condition. The one
                // exception: after a terminal `Done` this teardown already
                // accounted for (above, or in the main loop), the reader's
                // own drop-guard `ReaderGone` is that event's EXPECTED
                // trailer, not a second anomaly.
                if leg.output_ended_early.is_none() {
                    return Err(Error::State(
                        "capsule_win: the reader thread ended without a terminal Done event during reap".into(),
                    ));
                }
            }
        }
    }
    flush_output!(leg.w);

    // Phase B: close the pseudoconsole on a DEDICATED thread so THIS loop
    // can keep draining `output_rx` (feeding `handle_output!`, answering
    // the handshake) CONCURRENTLY with the close — the documented call
    // pattern ("reader already draining, THEN call this") applied
    // literally: draining must never itself pause to make the call. Both a
    // graceful EOF and a broken-pipe error are the ORDINARY, expected end
    // of this drain (the close is what produces them) — unlike Phase A's
    // identical-looking check, neither is an anomaly here.
    let closer_handle = leg.producer.close_output_side();
    // The close itself is UNCONDITIONAL -- it drops the held slave (a
    // real close(2), still owed on a revoked fd), keeps `Drop`
    // idempotent, and yields the `closer_handle` the aggregate join
    // below needs. Only the DRAIN is guarded: when the output side
    // already reached its terminal state before this point (see the
    // arms above), there is no EOF left for this loop to wait out, and
    // waiting for one would burn `TEARDOWN_DRAIN_TIMEOUT` and then fail
    // a run that is in fact complete.
    if leg.output_ended_early.is_none() {
        let drain_deadline = Instant::now() + TEARDOWN_DRAIN_TIMEOUT;
        loop {
            service_transport_events_teardown!();
            execute_teardown_actions!(leg.attach_proto.tick(Instant::now()));
            eager_ground_check!();
            // Codex review (PR #227): checked here, unconditionally, every
            // iteration — mirroring Phase A's `reap_deadline` just above and
            // the main loop's own commit-deadline fix — rather than only
            // inside the `Timeout` arm below, where continuous transport
            // activity could starve it exactly as it did the commit deadline.
            if Instant::now() >= drain_deadline {
                return Err(Error::State(
                    "capsule_win: reader did not reach EOF within the teardown drain timeout".into(),
                ));
            }
            match leg.output_rx.recv_timeout(TEARDOWN_DRAIN_POLL) {
                Ok(ReaderEvent::Output(bytes)) => {
                    pace_output!(bytes);
                    maybe_rotate!(leg.w);
                }
                Ok(ReaderEvent::TransportActivity) => {
                    // Switch-latency Phase 1 (c): same wake as both other
                    // sites -- see the main loop's own arm.
                    leg.wake_pending.store(false, Ordering::Release);
                }
                Ok(ReaderEvent::Done(_)) => {
                    // Round-2 review, finding 5: service transport ONE more
                    // time at the exact instant EOF ends this drain, so a
                    // status/mgmt request that arrived just after the last
                    // loop-top poll still gets answered while the pipe is
                    // provably still live -- without this, everything from
                    // here to `shutdown_all`'s eventual close (the flush and
                    // joins below, the exit-status wait, writing lifecycle
                    // state, sealing) is a live-but-unserviced pipe tail.
                    service_transport_events_teardown!();
                    execute_teardown_actions!(leg.attach_proto.tick(Instant::now()));
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Ok(ReaderEvent::ReaderGone) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // Codex review (PR #227): see the main loop's identical arm.
                    return Err(Error::State(
                        "capsule_win: the reader thread ended without a terminal Done event during drain".into(),
                    ));
                }
            }
        }
    } else {
        // The one thing the skipped drain's own EOF arm owes is the
        // final transport service at the EOF instant (round-2 finding
        // 5) -- pay it here instead, so a mgmt request that arrived
        // just after the last loop-top poll is still answered while the
        // pipe is provably live.
        service_transport_events_teardown!();
        execute_teardown_actions!(leg.attach_proto.tick(Instant::now()));
    }
    flush_output!(leg.w);

    // U1a, EndRun state machine item 4 / ack grace: the FINAL service poll
    // just above (the one at the exact EOF instant) can itself have
    // admitted a NEW mgmt `shutdown` and queued its `ShutdownAck` — give
    // that specific send up to `SHUTDOWN_ACK_GRACE` to be reported
    // physically written (removing its entry from `pending_sends`) before
    // this capsule's own transport goes away.
    //
    // U1a Codex round-1, Major 6 discharge: this window drains ONLY what
    // is ALREADY pending (`Sent`/`ConnectionClosed`, via
    // `drain_pending_sends_only!`) — it admits NOTHING new
    // (`ConnectionOpened`/fresh `Bytes` are closed outright, never reaching
    // `attach_proto`). A request newly accepted with, say, 50ms left in
    // this window would have almost no time to get its own ack physically
    // written, contradicting the "final service poll" guarantee this grace
    // exists to honor — so after the ordinary teardown drain ends, no
    // request is newly admitted at all; only what is already outstanding
    // (the ack this grace exists for, or any other send already queued
    // when the drain ended) gets to finish.
    let shutdown_ack_deadline = Instant::now() + SHUTDOWN_ACK_GRACE;
    while leg.pending_sends
        .values()
        .any(|m| matches!(m, Some(SentMarker::ShutdownAck { .. })))
        && Instant::now() < shutdown_ack_deadline
    {
        drain_pending_sends_only!();
        execute_teardown_actions!(leg.attach_proto.tick(Instant::now()));
        std::thread::sleep(SHUTDOWN_ACK_GRACE_POLL);
    }
    // The pipe's own disappearance: explicit HERE, rather than only
    // whenever `run` happens to return next (the exit-status wait and the
    // seal below need no pipe at all) — ADR 0041's grace is specifically
    // about DEFERRING that disappearance until it resolves, which requires
    // an actual close at THIS point, not a hope that returning soon is soon
    // enough. `shutdown_all` is idempotent (U1a): `ShutdownGuard`'s own
    // `Drop`, still ahead on every path, is a safe no-op the second time.
    //
    // Codex round-1 Blocker 3 discharge: ONE absolute aggregate deadline,
    // shared by the transport's OWN internal joins (accepted/reaper/every
    // connection worker, all cancellation-first per `Transport::
    // shutdown_all`'s own doc) AND this module's closer/reader threads —
    // "over an acceptor, a reaper, up to sixteen connection workers and
    // the capsule's own threads" (ADR 0041 bounds table). Cancellation for
    // THIS module's own threads already happened earlier in this same
    // function (Phase A's `producer.terminate_domain()`, Phase B's
    // `producer.close_output_side()` on `closer_handle`) — by this point
    // both threads are expected to be
    // at or near their own natural return, so `join_within` (never the
    // raw blocking `.join()`) is what actually bounds the residual gap
    // between "signalled EOF/exit" and "the thread function returned".
    // Expiry is TERMINAL: `run` must not seal-and-succeed, nor release the
    // writer fence (via `store`'s own drop), past a teardown that could
    // not prove every worker stopped — an `Err` here propagates before
    // `w.seal`/`store.advance_chain` are ever reached, and `store` (the
    // fence) still drops via its own destructor on this return path,
    // exactly as any other early `?` in this function already does.
    let teardown_deadline = Instant::now() + TEARDOWN_AGGREGATE_DEADLINE;
    let transport_ok = leg.transport.0.shutdown_all(teardown_deadline);
    let closer_ok = join_within(closer_handle, teardown_deadline);
    let reader_ok = join_within(reader_handle, teardown_deadline);
    if !(transport_ok && closer_ok && reader_ok) {
        return Err(Error::State(format!(
            "capsule_win: aggregate teardown did not complete within its {TEARDOWN_AGGREGATE_DEADLINE:?} \
             deadline (transport ok={transport_ok}, closer ok={closer_ok}, reader ok={reader_ok}); \
             refusing to seal or report success past an unproven teardown"
        )));
    }

    // Step 5: the producer's own exit status, raw and unsigned end-to-end
    // for the Windows `Code` case (review finding: a Unix-style `i32` cast
    // would turn a high-bit NTSTATUS-shaped code negative for no reason).
    // `wait()` first establishes the honesty-bound precondition
    // `exit_status_after_confirmed_exit`'s own doc requires —
    // `domain_is_empty` above already proved the process isn't running,
    // but this satisfies the bound by the letter of its doc, not just by
    // inference.
    if !leg.producer.wait(Duration::from_secs(5))? {
        return Err(Error::State(
            "capsule_win: producer did not signal after its domain reaped to empty".into(),
        ));
    }
    let exit_status = leg.producer.exit_status_after_confirmed_exit()?;

    // The mgmt `shutdown` reason, if that is what drove this EndRun (ADR
    // 0041: "the reason string is recorded in producer_dead's detail").
    // `producer_uptime_ms` (N1, captured well above, at the exit_kind
    // boundary -- NOT recomputed here, past all the teardown machinery
    // this point sits after) is an ADDITIVE, free-form diagnostic field
    // -- like `reason` already is -- not a registered ADR 0039 feature:
    // it changes no authority, so no segment needs to declare anything
    // to carry it, and an older reader simply ignores an unknown plain
    // JSON field, exactly as `detail` has always allowed.
    //
    // ADR 0043 decision 13: `Code(c)` writes the SAME `exit_code` (u32)
    // field this crate has always written (Windows never reaches the
    // other arm); `Signal(n)` is the additive Unix shape, `signal` (i32),
    // unreachable here.
    let mut detail = json!({ "producer_uptime_ms": producer_uptime_ms });
    match exit_status {
        ExitStatus::Code(c) => detail["exit_code"] = json!(c),
        ExitStatus::Signal(n) => detail["signal"] = json!(n),
    }
    if let Some(reason) = &leg.shutdown_reason {
        detail["reason"] = json!(reason);
    }
    // ADR 0043 decision 12, as amended: the output side ended before
    // teardown closed it AND the producer's own exit explained it inside
    // `READER_END_EXIT_GRACE` -- the one case the pre-close rule now admits,
    // and it is admitted RECORDED, never silently. Additive and free-form
    // exactly like `reason` above, absent unless that case occurred: on
    // macOS it is the kernel's revoke on the session leader's exit (where
    // the producer's last undrained output can be lost with it, which is
    // precisely why the record says so); on Linux and Windows the arms that
    // set it are unreachable, so no record written there ever carries it.
    if let Some(how) = &leg.output_ended_early {
        detail["output_ended_early"] = json!(how);
    }
    let f = leg.ctx.capsule_frame(Class::Lifecycle, json!({"kind": "producer_dead", "detail": detail}));
    leg.w.append(&f, Commit::Immediate)?;
    leg.frames_written += 1;

    let digest = leg.w.seal(None)?;
    leg.store.advance_chain(digest);
    leg.segments_sealed += 1;

    Ok(ExitSummary {
        exit_code: Some(exit_status),
        exit_kind,
        frames_written: leg.frames_written,
        segments_sealed: leg.segments_sealed,
        handshake_answered: leg.dsr_answered,
        handshake_suppressed_matches: leg.handshake_suppressed_matches,
        resize_os_calls: leg.resize_os_calls,
    })
}
