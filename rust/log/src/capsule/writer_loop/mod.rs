//! `run`: the capsule writer loop, one producer from spawn to its end.
use super::*;
use super::ended::{self, Ended};
use super::fence;
use super::screen::{self, ScreenFile};
use std::ops::ControlFlow;

mod lanes;
mod output_path;
mod phases;
mod start;
use phases::{ack_grace, drain_output, finish_run, join_workers, main_loop, reap_domain};
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

/// The leg's writer-loop state from `start` to `finish_run`: every value two or more of
/// `run`'s pieces share. Each piece takes it last, by `&mut` or by value.
///
/// FIELD ORDER IS LOAD-BEARING for the last five fields. Fields drop in declaration order: the
/// output budget is cancelled, the producer's domain dropped, the output channel closed, the
/// transport shut down, and the fence released last. No other field's drop does anything. Never
/// implement `Drop` for this struct.
struct Leg<'t, P> {
    /// The leg's folder: the fence's home, and where `ended` is written.
    voyage_root: PathBuf,
    /// The durable take epoch: the capsule's own, fed in and out of `attach_proto` as a plain value. Starts at 1 (the
    /// null holder); each committed take raises it.
    take_epoch: u64,
    /// The driver's controller id, `None` until the first take commits.
    holder: Option<String>,
    /// The attach protocol: it decides, the loop carries out its actions.
    attach_proto: AttachProto,
    /// Frame reassembly per open connection; a connection is active while it has one.
    splitters: HashMap<ConnId, wire::FrameSplitter>,
    /// Sends not yet reported written, by (connection, send id).
    pending_sends: HashMap<(ConnId, u64), Option<SentMarker>>,
    /// Ends the main loop at its next check (shutdown ack written, or transport failed).
    shutdown_requested: bool,
    /// The attach lane's transport failed for good: the run ends, not as a requested end.
    transport_failed: bool,
    /// The reason `ended` records; the first one set wins.
    shutdown_reason: Option<String>,
    /// An end was asked for and `ended` says so; never unset.
    run_end_latched: bool,
    /// Coalesces transport wakes; a loop clears it when it consumes one.
    wake_pending: Arc<AtomicBool>,
    /// The live terminal screen, for checkpoints, the ground gate and the screen file.
    parser: vt100_ctt::Parser,
    /// The screen file, when the leg keeps one.
    screen: Option<ScreenFile>,
    /// Finds the host DA1 query in producer output.
    handshake: HostHandshake,
    /// The first DA1 query was answered (`ExitSummary`).
    dsr_answered: bool,
    /// DA1 queries after the first, counted only (`ExitSummary`).
    handshake_suppressed_matches: u64,
    /// Resize calls that reached the producer (`ExitSummary`).
    resize_os_calls: u64,
    /// Tells the run's owner the agent started and wrote (`CapsuleConfig::signals`).
    signals: Option<Arc<RunSignals>>,
    /// The row socket's fan-out, when the owner serves one: fed every committed chunk, and asked for its requests every pass.
    row: Option<Arc<crate::row::RowRun>>,
    /// Typed input to the terminal, written off this loop's thread.
    input: crate::row::InputPump,
    /// An output end before teardown that the producer's exit explained; `ended` records it.
    output_ended_early: Option<String>,
    /// The reader's byte budget; the loop releases what it handles.
    output_budget: Arc<OutputBudget>,
    /// Output handled since the pacer last slept.
    bytes_since_yield: usize,
    /// Cancels the budget on any exit, so a reader blocked in `reserve` wakes. Drops first.
    _budget_guard: BudgetCancelGuard,
    /// The producer and its kill domain.
    producer: P,
    /// Producer output, the reader's end, and transport wakes: one channel.
    output_rx: mpsc::Receiver<ReaderEvent>,
    /// Shuts the transport down on any exit, while the fence is still held.
    transport: ShutdownGuard<'t>,
    /// The writer fence. Drops last.
    _fence: sot_platform::host::WriterLock,
}

/// Run one producer under a capsule, generic over `P: Producer`. Blocks until the run ends — either the producer
/// exits on its own, `commands` delivers [`Command::Kill`], or a mgmt
/// `shutdown` commits the durable EndRun latch. The latch starts teardown
/// without waiting for physical ack completion; the ack grace bounds
/// the wait for a pending shutdown acknowledgement (ADR 0041 EndRun).
/// The caller owns the `Sender` of `commands` (see the module doc's
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
    finish_run(exit_kind, producer_uptime_ms, leg)
}
