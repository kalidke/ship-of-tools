//! Starting a leg: take the fence, bind the transport, restore the screen, spawn the producer and its reader, and hand `run` the loop state.
use super::output_path::spawn_reader;
use super::*;

pub(super) fn start<'t, P: Producer>(
    config: &CapsuleConfig,
    transport: &'t mut dyn Transport,
) -> Result<ControlFlow<ExitSummary, (Leg<'t, P>, std::thread::JoinHandle<()>, Instant)>> {
    // Fail before any durable side effect: `self_status` can fail on Linux and macOS.
    #[cfg(not(windows))]
    let _ = self_status(config.survival)?;
    let fence = take_fence(config)?;
    // A former leg's `ended` is not this leg's: the supervisor reads it only after this leg ends. The leg's own
    // number is what a supervisor's status reports while it runs.
    ended::clear(&config.voyage_root)?;
    fence::next_leg_epoch(&config.voyage_root)?;

    // `shutdown_all` must run before the fence releases (`fence`'s own drop) on EVERY exit path from this point on,
    // not only the success one: an RAII guard is the only way to guarantee that whichever `?` returns early below.
    // Declared AFTER `fence`, so this guard's `Drop` (closing the pipe) runs BEFORE the fence's release. Every
    // fallible operation from here on that runs while the fence is held stays AFTER the guard.
    //
    // This guard is never explicitly disarmed. The ack-grace call site calls `transport.0.shutdown_all()` directly
    // once its window resolves, so the pipe disappears promptly rather than staying live through the remaining
    // process-exit wait; THIS Drop then calls it again regardless, on every exit path. That second call is safe only
    // because `Transport::shutdown_all` is a documented idempotent contract (see that method's own doc).
    let mut transport = ShutdownGuard(transport);

    let (tx, output_rx, wake_pending) = bind_transport(config, &mut transport)?;

    // The attach protocol: platform-neutral state machine (`attach_proto`);
    // `pid`/`created` are OS values it must never compute itself.
    let attach_proto = AttachProto::new(self_status(config.survival)?);
    let splitters: HashMap<ConnId, wire::FrameSplitter> = HashMap::new();
    // Keyed by (conn, id), not id alone: a transport's send ids are only ever meaningful scoped to the connection
    // that issued them, and every entry for a connection is purged the moment it closes (see `execute_light_actions`'s
    // `Close` arm), so a canceled write can never leak an entry, nor can a stale completion apply a marker meant for a
    // connection that no longer exists.
    let pending_sends: HashMap<(ConnId, u64), Option<SentMarker>> = HashMap::new();

    // The live parser, bounded scrollback (`CAPSULE_SCROLLBACK_ROWS`): the ring rides in the checkpoint (vt100 fork
    // format version 2) and a restore REPLACES the client's ring rather than appending, so re-attaching does not
    // double it. A screen file a former leg left starts it, under a restart line.
    let mut parser = vt100_ctt::Parser::new(config.rows, config.cols, CAPSULE_SCROLLBACK_ROWS);
    let screen = config.screen_path.clone().map(|path| {
        screen::restore(&path, &mut parser, config.rows, config.cols, SystemTime::now());
        ScreenFile::new(path, Instant::now())
    });

    let spawn_result = spawn_producer::<P>(config);
    let mut producer = match spawn_result {
        Ok(p) => p,
        Err(reason) => return spawn_failure(config, reason).map(ControlFlow::Break),
    };
    // The supervisor's anti-flap counter judges stability on the PRODUCER's lifetime, never on how long this
    // process's own teardown happens to take afterward. Captured the instant a real spawn succeeds (not before the
    // attempt, which would count spawn latency as producer uptime).
    let spawned_at = Instant::now();

    let handshake = HostHandshake::new();

    // The output budget, and the guard that cancels it on ANY exit from this function from this point on, declared
    // as early as the budget itself so an early `?` anywhere below unwinds through it.
    let output_budget = Arc::new(OutputBudget::new());
    let _budget_guard = BudgetCancelGuard(Arc::clone(&output_budget));

    let reader_handle = spawn_reader(&mut producer, &output_budget, tx);

    // Loop-fairness MITIGATION, not a guarantee (real CI failure, windows-latest only: an attach timed out waiting on
    // a connection the capsule itself closed with `PreAdmissionTimeout`, even though the test's `hello` had already
    // arrived on the wire). `output_rx.recv_timeout` never blocks, and so never yields this thread, for as long as
    // output keeps arriving faster than the timeout; on a CPU-constrained runner with a fast enough producer that
    // starves whatever OS thread delivers transport bytes. `pace_output` sleeps 1 ms per `PACE_BYTES` of output
    // processed; a timed sleep is a real scheduling point on every Windows build (a `yield_now` was not). The cost is
    // bounded: 1 ms per 256 KiB caps output throughput near 250 MiB/s, two orders of magnitude above anything conhost
    // delivers. Nothing about protocol ordering depends on it.
    let bytes_since_yield: usize = 0;
    Ok(ControlFlow::Continue((
        Leg {
            voyage_root: config.voyage_root.clone(),
            take_epoch: 1,
            holder: None,
            attach_proto,
            splitters,
            pending_sends,
            shutdown_requested: false,
            shutdown_reason: None,
            run_end_latched: false,
            wake_pending,
            parser,
            screen,
            handshake,
            dsr_answered: false,
            handshake_suppressed_matches: 0,
            resize_os_calls: 0,
            // Set by the ONE rule in the main loop's arm and teardown Phase A's identical one: a terminal reader
            // event that arrived before `close_output_side` and that a confirmed producer exit explained. Stays
            // `None` on Linux and on Windows by those platforms' own contracts.
            output_ended_early: None,
            output_budget,
            bytes_since_yield,
            _budget_guard,
            producer,
            output_rx,
            transport,
            _fence: fence,
        },
        reader_handle,
        spawned_at,
    )))
}

/// Takes the fence of the leg's folder, making the folder first if it is not there. The parent-death lease is checked
/// once, as the fence's first act: a broken one releases the fence and ends the leg before it binds anything.
///
/// The lease's own open is deferred into the closure, so it happens at the exact point `take_fence` calls it, once.
/// `NamedMutex` (Windows) folds an unopenable lease to broken, as `supervisor::lease_win::open` documents; `InheritedFd`
/// (Unix) does one non-blocking read on the inherited descriptor: `EAGAIN` is alive, anything else (a missing
/// descriptor included) is broken.
fn take_fence(config: &CapsuleConfig) -> Result<sot_platform::host::WriterLock> {
    let root = sot_platform::host::ensure_container(&config.voyage_root)?;
    if !root.exists() {
        fence::bootstrap(&root)?;
    }
    let lease_broken_fn = {
        let lease = config.parent_lease.clone();
        move || match &lease {
            None => false,
            #[cfg(windows)]
            Some(ParentLease::NamedMutex(name)) => {
                crate::supervisor::lease_win::open(name).map(|c| c.is_broken()).unwrap_or(true)
            }
            #[cfg(unix)]
            Some(ParentLease::InheritedFd(fd)) => crate::capsule::producer::pty::parent_lease_fd_broken(*fd),
        }
    };
    let lease_broken: Option<&dyn Fn() -> bool> = config.parent_lease.is_some().then_some(&lease_broken_fn);
    fence::take_fence(&root, lease_broken)
}

fn bind_transport(
    config: &CapsuleConfig,
    transport: &mut ShutdownGuard<'_>,
) -> Result<(mpsc::Sender<ReaderEvent>, mpsc::Receiver<ReaderEvent>, Arc<AtomicBool>)> {
    // Switch-latency Phase 1 (c): ONE channel for producer output, the
    // reader thread's own death (`ReaderGone`), and
    // transport activity — the reader thread (spawned later once
    // `producer` exists) and its own `ReaderGoneGuard` are two of this
    // channel's three senders, so this loop's own `output_rx.
    // recv_timeout` wait (below) wakes on any of the three without a
    // second channel or a select. Created here, ahead of `bind`
    // (`Transport::set_wake`'s own contract: register before binding)
    // rather than at the reader thread's own spot further down.
    let (tx, output_rx) = mpsc::channel::<ReaderEvent>();
    // Coalescing: several transport events arriving between one drain
    // and the next collapse into ONE queued wake, cleared the moment the
    // loop actually consumes a `TransportActivity` (below) — a busy
    // transport can never grow an unbounded backlog of these stacked on
    // top of the real, separately-bounded queue `try_recv_event` drains.
    let wake_pending = Arc::new(AtomicBool::new(false));
    transport.0.set_wake({
        let wake_tx = tx.clone();
        let wake_pending = Arc::clone(&wake_pending);
        Arc::new(move || {
            if !wake_pending.swap(true, Ordering::AcqRel) {
                // The peer send failing here means the loop already
                // dropped `output_rx` (this run is past the point of
                // caring) -- never a reason to panic a transport worker
                // thread over.
                let _ = wake_tx.send(ReaderEvent::TransportActivity);
            }
        })
    });

    // The pipe-lifetime invariant, enforced here by code order (see
    // `Transport::bind`'s own doc): the writer lock is already held
    // (`store`, above) and `ShutdownGuard` is already in place to close
    // whatever `bind` DID manage to set up on any later early return, so
    // `bind` runs before any OTHER fallible step gets a chance to leave
    // the lock held with the transport in a half-set-up state.
    transport.0.bind(&config.voyage_id)?;
    Ok((tx, output_rx, wake_pending))
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
