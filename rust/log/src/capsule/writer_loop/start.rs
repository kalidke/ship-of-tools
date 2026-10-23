//! Starting a leg: take the fence, restore the screen, spawn the producer and its reader, and hand `run` the loop state.
use super::output_path::spawn_reader;
use super::*;

pub(super) fn start<P: Producer>(
    config: &CapsuleConfig,
) -> Result<ControlFlow<ExitSummary, (Leg<P>, std::thread::JoinHandle<()>, Instant)>> {
    let fence = take_fence(config)?;
    // A former leg's `ended` is not this leg's: the supervisor reads it only after this leg ends. The leg's own
    // number is what a supervisor's status reports while it runs.
    ended::clear(&config.voyage_root)?;
    fence::next_leg_epoch(&config.voyage_root)?;

    // One channel for producer output, the reader thread's own death (`ReaderGone`) and the row's wakes: the reader
    // thread (spawned below once `producer` exists) and its own `ReaderGoneGuard` are two of this channel's senders and
    // the row's wake callback the third, so this loop's `output_rx.recv_timeout` wakes on any of them without a second
    // channel or a select.
    let (tx, output_rx) = mpsc::channel::<ReaderEvent>();

    // The live parser, bounded scrollback (`CAPSULE_SCROLLBACK_ROWS`): the ring rides in the checkpoint (vt100 fork
    // format version 2) and a restore REPLACES the client's ring rather than appending, so re-attaching does not
    // double it. A screen file a former leg left starts it, under a restart line.
    let mut parser = vt100_ctt::Parser::new(config.rows, config.cols, CAPSULE_SCROLLBACK_ROWS);
    let screen = config.screen_path.clone().map(|path| {
        let restored = screen::restore(&path, &mut parser, config.rows, config.cols, SystemTime::now());
        let mut file = ScreenFile::new(path, Instant::now());
        if restored {
            // The restart line changed the screen: a producer that stays silent still saves it.
            file.output_arrived();
        }
        file
    });

    let row = config.signals.as_ref().and_then(|s| s.row());
    if let Some(row) = &row {
        // A request on the row socket wakes the loop.
        let wake_tx = tx.clone();
        row.set_wake(Box::new(move || {
            let _ = wake_tx.send(ReaderEvent::Wake);
        }));
    }

    let spawn_result = spawn_producer::<P>(config);
    let mut producer = match spawn_result {
        Ok(p) => p,
        Err(reason) => {
            if let Some(row) = &row {
                row.end_run();
            }
            return spawn_failure(config, reason).map(ControlFlow::Break);
        }
    };
    // The supervisor's anti-flap counter judges stability on the PRODUCER's lifetime, never on how long this
    // process's own teardown happens to take afterward. Captured the instant a real spawn succeeds (not before the
    // attempt, which would count spawn latency as producer uptime).
    let spawned_at = Instant::now();
    if let Some(signals) = &config.signals {
        signals.spawned(spawned_at);
    }

    let handshake = HostHandshake::new();
    let input = crate::row::InputPump::new(producer.input_writer().map_err(crate::Error::Io)?).map_err(crate::Error::Io)?;

    // The output budget, and the guard that cancels it on ANY exit from this function from this point on, declared
    // as early as the budget itself so an early `?` anywhere below unwinds through it.
    let output_budget = Arc::new(OutputBudget::new());
    let _budget_guard = BudgetCancelGuard(Arc::clone(&output_budget));

    let reader_handle = spawn_reader(&mut producer, &output_budget, tx);

    // Loop-fairness MITIGATION, not a guarantee (real CI failure, windows-latest only: an attach timed out waiting on
    // a connection the capsule itself closed with `PreAdmissionTimeout`, even though the test's `hello` had already
    // arrived on the wire). `output_rx.recv_timeout` never blocks, and so never yields this thread, for as long as
    // output keeps arriving faster than the timeout; on a CPU-constrained runner with a fast enough producer that
    // starves whatever OS thread delivers a row connection's bytes. `pace_output` sleeps 1 ms per `PACE_BYTES` of output
    // processed; a timed sleep is a real scheduling point on every Windows build (a `yield_now` was not). The cost is
    // bounded: 1 ms per 256 KiB caps output throughput near 250 MiB/s, two orders of magnitude above anything conhost
    // delivers. Nothing about protocol ordering depends on it.
    let bytes_since_yield: usize = 0;
    Ok(ControlFlow::Continue((
        Leg {
            voyage_root: config.voyage_root.clone(),
            shutdown_reason: None,
            run_end_latched: false,
            parser,
            screen,
            handshake,
            dsr_answered: false,
            handshake_suppressed_matches: 0,
            resize_os_calls: 0,
            signals: config.signals.clone(),
            row,
            input,
            // Set by the ONE rule in the main loop's arm and teardown Phase A's identical one: a terminal reader
            // event that arrived before `close_output_side` and that a confirmed producer exit explained. Stays
            // `None` on Linux and on Windows by those platforms' own contracts.
            output_ended_early: None,
            output_budget,
            bytes_since_yield,
            _budget_guard,
            producer,
            output_rx,
            _fence: fence,
        },
        reader_handle,
        spawned_at,
    )))
}

/// Takes the fence of the leg's folder, making the folder first if it is not there.
fn take_fence(config: &CapsuleConfig) -> Result<sot_platform::host::WriterLock> {
    let root = sot_platform::host::ensure_container(&config.voyage_root)?;
    if !root.exists() {
        fence::bootstrap(&root)?;
    }
    fence::take_fence(&root)
}

fn spawn_producer<P: Producer>(config: &CapsuleConfig) -> std::result::Result<P, String> {
    // Initial geometry validated by the SAME rule a resize is (ADR 0041).
    // An out-of-budget request here is treated exactly like a spawn
    // failure: nothing was ever created, so the same compensation path
    // applies — no separate code path needed for "never even tried".
    let geometry_ok =
        (MIN_COLS..=MAX_COLS).contains(&config.cols) && (MIN_ROWS..=MAX_ROWS).contains(&config.rows);
    if !geometry_ok {
        Err(format!(
            "initial geometry {}x{} outside the 2x2..512x256 budget",
            config.cols, config.rows
        ))
    } else {
        P::spawn(&config.argv, config.cols, config.rows).map_err(|e| e.to_string())
    }
}

/// The producer never ran: say so in `ended` (nothing was asked for, no uptime) and end honestly, rather than the bare
/// `?` escape that would leave the supervisor nothing to read.
fn spawn_failure(config: &CapsuleConfig, reason: String) -> Result<ExitSummary> {
    ended::write(
        &config.voyage_root,
        &Ended { finished: true, reason: Some(format!("spawn_failed: {reason}")), ..Ended::default() },
    )?;
    Ok(ExitSummary {
        exit_code: None,
        exit_kind: ExitKind::SpawnFailed,
        handshake_answered: false,
        handshake_suppressed_matches: 0,
        resize_os_calls: 0,
    })
}
