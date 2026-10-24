//! `run`: the capsule writer loop, one producer from spawn to its end.
use super::*;
use super::ended::{self, Ended};
use super::screen::{self, ScreenFile};
use std::ops::ControlFlow;

mod output_path;
mod phases;
mod row_requests;
mod start;
use phases::{drain_output, finish_run, join_workers, main_loop, reap_domain};
use start::start;

/// The leg's writer-loop state from `start` to `finish_run`: every value two or more of
/// `run`'s pieces share. Each piece takes it last, by `&mut` or by value.
///
/// FIELD ORDER IS LOAD-BEARING for the last three fields. Fields drop in declaration order: the
/// output budget is cancelled, the producer's domain dropped, and the output channel closed last.
/// No other field's drop does anything. Never implement `Drop` for this struct.
struct Leg<P> {
    /// The row's state folder: where `ended` is written.
    state_dir: PathBuf,
    /// The reason `ended` records; the first one set wins.
    shutdown_reason: Option<String>,
    /// An end was asked for and `ended` says so; never unset.
    run_end_latched: bool,
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
    /// Tells the run's owner the agent lived long enough to be ready (`CapsuleConfig::signals`).
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
    /// Producer output, the reader's end, and the row's wakes: one channel.
    output_rx: mpsc::Receiver<ReaderEvent>,
}

/// Run one producer under a capsule, generic over `P: Producer`. Blocks until the run ends: either the producer
/// exits on its own, or `commands` delivers [`Command::Kill`] or [`Command::Stop`]. The caller owns the `Sender` of
/// `commands` (see the module doc's stdin-ownership point). The row socket the run feeds is the caller's, named by
/// `config.signals`.
pub fn run<P: Producer>(config: CapsuleConfig, commands: mpsc::Receiver<Command>) -> Result<ExitSummary> {
    let (leg, reader_handle, spawned_at) = match start::<P>(&config)? {
        ControlFlow::Continue(running) => running,
        ControlFlow::Break(summary) => return Ok(summary),
    };
    let (leg, exit_kind) = main_loop(&commands, leg)?;
    // The run is over for the row socket: its queued requests are answered `run ended` and no later one is admitted.
    if let Some(row) = &leg.row {
        row.end_run();
    }
    // Captured HERE, the instant the main loop concludes for either exit kind, not after the teardown machinery
    // below (job reap, ConPTY drain, the aggregate deadline, a final wait), which alone can outlast the producer's own
    // life. For ProducerExited the producer is already dead by definition; for Requested it is about to be forcibly
    // killed by `producer.terminate_domain()` a few lines into teardown, with no intervening I/O between here and
    // there.
    let producer_uptime_ms = u64::try_from(spawned_at.elapsed().as_millis()).unwrap_or(u64::MAX);

    let leg = reap_domain(leg)?;
    let (mut leg, closer_handle) = drain_output(leg)?;
    join_workers(closer_handle, reader_handle, &mut leg)?;
    finish_run(exit_kind, producer_uptime_ms, leg)
}
