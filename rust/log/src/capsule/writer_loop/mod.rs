//! `run`: the capsule writer loop, one producer from spawn to sealed voyage.
use super::*;
use std::ops::ControlFlow;

mod lanes;
mod output_path;
mod phases;
mod start;
use output_path::flush_output;
use phases::{ack_grace, drain_output, join_workers, main_loop, reap_domain, seal_run};
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
            leg_note(format_args!(
                "sot-capsule: ShutdownGuard's fallback teardown did not complete within its \
                 aggregate deadline; a worker thread may still be running"
            ));
        }
    }
}

/// One diagnostic line, as one write that never panics: `eprintln!` panics
/// when stderr cannot take the line, and a full volume under the leg's log
/// would then turn a diagnostic into a crash of the leg.
fn leg_note(args: std::fmt::Arguments<'_>) {
    let line = format!("{args}\n");
    let _ = std::io::Write::write_all(&mut std::io::stderr(), line.as_bytes());
}

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
/// exits on its own, `commands` delivers [`Command::Kill`], or a mgmt
/// `shutdown` commits the durable EndRun latch. The latch starts teardown
/// without waiting for physical ack completion; the ack grace bounds
/// the wait for a pending shutdown acknowledgement (ADR 0041 EndRun).
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
    let (leg, reader_handle, spawned_at) = match start::<P>(&config, transport)? {
        ControlFlow::Continue(running) => running,
        ControlFlow::Break(summary) => return Ok(summary),
    };
    let (mut leg, exit_kind) = main_loop(&commands, leg)?;
    // captured HERE, the
    // instant the main loop concludes for EITHER exit kind -- NOT after
    // the teardown machinery below (job reap, ConPTY drain, the
    // aggregate deadline, a final wait), which alone can outlast the
    // producer's own life and would otherwise pollute this measurement
    // with exactly the capsule-side latency the supervisor's own
    // anti-flap counter must never see. For
    // ProducerExited the producer is already dead by definition; for
    // Requested it is about to be forcibly killed by
    // `producer.terminate_domain()` a few lines into teardown, with no
    // intervening I/O between here and there.
    let producer_uptime_ms = u64::try_from(spawned_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    flush_output(&mut leg)?;

    // Producer-bound admission (take/input/resize) is revoked from here on
    // — but mgmt (probe/status/shutdown) and Sent completions
    // keep being serviced through BOTH phases below, via
    // `service_transport_events_teardown`, until the pipe closes (see that
    // function's own doc for why, and `execute_teardown_actions` for the
    // reduced action set this implies).
    leg.attach_proto.begin_teardown();

    leg = reap_domain(leg)?;
    let (mut leg, closer_handle) = drain_output(leg)?;
    ack_grace(&mut leg)?;
    join_workers(closer_handle, reader_handle, &mut leg)?;
    seal_run(exit_kind, producer_uptime_ms, leg)
}
