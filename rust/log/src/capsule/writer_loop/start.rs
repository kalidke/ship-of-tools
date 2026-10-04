//! Starting a leg: open the voyage, bind the transport, write the control preamble, spawn the producer and its reader, and hand `run` the loop state.
use super::output_path::spawn_reader;
use super::*;

pub(super) fn start<'t, P: Producer>(
    config: &CapsuleConfig,
    transport: &'t mut dyn Transport,
) -> Result<ControlFlow<ExitSummary, (Leg<'t, P>, std::thread::JoinHandle<()>, Instant)>> {
    #[cfg(not(windows))]
    let _ = self_status(config.survival)?;
    let mut store = open_store(config)?;

    // `shutdown_all` must run
    // before the writer lock releases (`store`'s own drop) on EVERY exit
    // path from this point on, not only the success one -- an RAII guard
    // is the only way to guarantee that regardless of which `?` returns
    // early below. Declared AFTER `store`: this guard's `Drop` (closing the pipe) runs
    // BEFORE `store`'s (releasing the lock). Constructed HERE, immediately
    // after `store` itself and BEFORE `seal_survivor()?`. Every fallible operation from this point on that
    // runs while the lock is held must stay AFTER the guard, not before
    // it.
    //
    // U1a: this guard is never explicitly DISARMED. The ack-grace call site
    // below calls `transport.0.shutdown_all()` directly once its window
    // resolves, so the pipe disappears promptly rather than staying live
    // through the remaining process-exit wait and seal; THIS Drop then
    // calls it again regardless, on every exit path, exactly as before.
    // That second call is safe only because `Transport::shutdown_all` is
    // now a documented idempotent contract (see that method's own doc) --
    // disarming this guard with a boolean flag would be the OTHER way to
    // make the double call safe, but it is strictly more machinery for the
    // same guarantee an idempotent method already gives for free.
    let mut transport = ShutdownGuard(transport);

    let (tx, output_rx, wake_pending) = bind_transport(config, &mut transport)?;

    let (mut ctx, segment_features, mut w) = open_first_segment(config, &mut store)?;
    let seg_bytes: u64 = 0;
    let mut frames_written: u64 = 0;
    let segments_sealed: u64 = 0;

    write_take_state(&store, &mut ctx, &mut w, &mut frames_written)?;

    // The attach protocol: platform-neutral state machine (`attach_proto`);
    // `pid`/`created` are OS values it must never compute itself.
    let attach_proto = AttachProto::new(self_status(config.survival)?);
    let splitters: HashMap<ConnId, wire::FrameSplitter> = HashMap::new();
    // keyed by (conn, id), not id alone -- a transport's send
    // ids are only ever meaningful scoped to the connection that issued
    // them (a real transport may recycle ids across connections), and
    // every entry for a connection is purged the moment it closes (see
    // `execute_light_actions`'s `Close` arm), so a canceled write can
    // never leak an entry, nor can a stale/mismatched completion apply a
    // marker meant for a connection that no longer exists.
    let pending_sends: HashMap<(ConnId, u64), Option<SentMarker>> = HashMap::new();
    let shutdown_requested = false;
    let shutdown_reason: Option<String> = None;
    // ADR 0041 EndRun step 2: IRREVOCABLE once true — never unset by
    // anything past this point (a stalled ack, a stopped-reading client,
    // a progress-deadline close, or a lost connection). Distinct from
    // `shutdown_requested`, which governs when TEARDOWN starts (only
    // once the ack ships); this one governs whether the durable marker
    // has already been committed, so a second concurrent `shutdown`
    // request writes no second frame (step 4).
    let run_end_latched = false;

    write_producer_attached(config, &mut ctx, &mut w, &mut frames_written)?;

    write_producer_spawn::<P>(config, &mut ctx, &mut w, &mut frames_written)?;

    let spawn_result = spawn_producer::<P>(config);

    let mut producer = match spawn_result {
        Ok(p) => p,
        Err(reason) => return seal_spawn_failure(reason, ctx, w, &mut store, frames_written, segments_sealed)
            .map(ControlFlow::Break),
    };
    // the supervisor's own anti-flap counter
    // must judge stability on the PRODUCER's lifetime, never on how long
    // this capsule process's own teardown (job reap, ConPTY drain,
    // aggregate deadline, final wait) happens to take afterward -- those
    // are all supervisor-invisible-until-exit timers that can alone
    // exceed the stability interval regardless of how long the producer
    // itself actually ran. Captured HERE, the instant a real spawn
    // succeeds (not before the attempt, which would count spawn latency
    // itself as producer uptime) -- `Instant` is `Copy`, so this survives
    // unmoved all the way to the `producer_dead` frame far below.
    let spawned_at = Instant::now();

    // Live vt100 parser (ADR 0041 "Terminal state") — kept current for a
    // later attach (step 5) to checkpoint from; this unit never serializes
    // it. Bounded scrollback (`CAPSULE_SCROLLBACK_ROWS`): a local capsule
    // pane could not be scrolled after attach, because every checkpoint
    // carried the visible screen only and the client's own ring started
    // empty on every attach. The ring rides in the checkpoint now (vt100
    // fork format version 2) and a restore REPLACES the client's ring
    // rather than appending, so re-attaching does not double it.
    let parser = vt100_ctt::Parser::new(config.rows, config.cols, CAPSULE_SCROLLBACK_ROWS);
    let handshake = HostHandshake::new();
    // ADR 0041's model is ONE host handshake, at startup — answer and
    // record only the first match ever observed; count the rest.
    let dsr_answered = false;
    let handshake_suppressed_matches: u64 = 0;
    let resize_os_calls: u64 = 0;

    // The output budget, and the guard that cancels it on ANY exit from
    // this function from this point on — declared as early as the budget
    // itself so an early `?` anywhere below unwinds through it.
    let output_budget = Arc::new(OutputBudget::new());
    let _budget_guard = BudgetCancelGuard(Arc::clone(&output_budget));

    let reader_handle = spawn_reader(&mut producer, &output_budget, tx);

    let pending_output: Vec<u8> = Vec::new();
    let pending_bytes: usize = 0;
    let last_commit = Instant::now();
    let last_fsync = Instant::now();
    let last_output = Instant::now();
    // Loop-fairness MITIGATION, not a guarantee (real CI failure, windows-
    // latest only: `attach_mid_stream_checkpoint_reproduces_reference_
    // screen` timed out waiting on a connection the capsule itself closed
    // with `PreAdmissionTimeout`, even though the test's `hello` had
    // already arrived on the wire). Root cause: `output_rx.recv_timeout`
    // below never actually blocks -- and so never yields this thread --
    // for as long as output keeps arriving faster than the timeout; on a
    // CPU-constrained runner with a fast enough producer (windows-latest's
    // conhost measured ~2x windows-2022's), that starves whatever OS
    // thread delivers transport bytes for this connection, long enough for
    // the unrelated `PRE_ADMISSION_TIMEOUT` deadline to fire on a `hello`
    // the loop never got a chance to even see.
    //
    // `pace_output` sleeps 1 ms per `GROUP_COMMIT_BYTES` of output
    // processed. The first version used `yield_now` (`SwitchToThread`),
    // which offers a ready thread ON THE CURRENT PROCESSOR a chance and
    // may return without switching -- and the starvation it was meant to
    // cure RECURRED on the faster CI image: with two hot threads (this
    // loop + the ConPTY reader) on a two-core runner, the thread that
    // must run to DELIVER a transport event never got a core, and
    // `PreAdmissionTimeout` fired on a hello that had already been sent.
    // A timed sleep is a real scheduling point on every Windows build:
    // the OS will run other ready threads for the duration. The cost is
    // bounded and named: 1 ms per 256 KiB caps output throughput near
    // 250 MiB/s -- two orders of magnitude above anything conhost
    // delivers -- and at real delivery rates the pacer fires rarely.
    // Nothing about protocol ordering or durability depends on this:
    // `service_transport_events`/`tick` already run once per iteration,
    // every write is fsynced before it's published (the watermark
    // barrier), and `attach_proto`'s replay tests prove the protocol
    // correct independent of timing.
    //
    // No deterministic unit test pins this one: `output_rx` (and the
    // reader thread feeding it) is constructed a few lines below, entirely
    // INSIDE this function, over a real spawned ConPTY -- unlike
    // `commands`/`transport`, it is not a parameter a test can substitute
    // a synthetic, saturated source for. The two real
    // Windows CI legs (windows-2022, windows-latest) are the pin for this
    // specific behavior until `run` grows an injectable output source
    // worth the refactor.
    let bytes_since_yield: usize = 0;
    // Set by the ONE rule below (both the main loop's arm and teardown
    // Phase A's identical one): a terminal reader event that arrived before
    // `close_output_side` and that a confirmed producer exit explained. Stays
    // `None` on Linux and on Windows by those platforms' own contracts -- see
    // the arm itself -- and its presence in `producer_dead.detail` is how the
    // one admitted case is RECORDED rather than forgiven.
    let output_ended_early: Option<String> = None;
    Ok(ControlFlow::Continue((
        Leg {
            ctx,
            segment_features,
            seg_bytes,
            frames_written,
            segments_sealed,
            attach_proto,
            splitters,
            pending_sends,
            shutdown_requested,
            shutdown_reason,
            run_end_latched,
            wake_pending,
            parser,
            handshake,
            dsr_answered,
            handshake_suppressed_matches,
            resize_os_calls,
            output_ended_early,
            output_budget,
            pending_output,
            pending_bytes,
            last_commit,
            last_fsync,
            last_output,
            bytes_since_yield,
            _budget_guard,
            producer,
            w,
            output_rx,
            transport,
            store,
        },
        reader_handle,
        spawned_at,
    )))
}


fn open_store(config: &CapsuleConfig) -> Result<VoyageStore> {
    // Resolve ONCE — the fresh `producer_pty`/`socket_transport` pair on
    // Linux and `producer_conpty`/`pipe_transport` on Windows share this
    // exact ordering (voyage root, then the lease, then the writer fence).
    let voyage_root = crate::host::ensure_container(&config.voyage_root)?;
    if !voyage_root.exists() {
        VoyageStore::bootstrap(&voyage_root, &config.voyage_id, config.retention)?;
    }
    // The lease OPEN itself is deferred to inside this closure, so it
    // happens lazily at the exact point `open_prepared` calls it EXACTLY
    // ONCE -- immediately after the writer fence is acquired, before any
    // other pre-fence-adjacent I/O -- never before. ADR 0043 decision 15:
    // per-platform variant of `ParentLease` — `NamedMutex` (Windows) folds
    // an unopenable/broken name to `true` (broken) here, matching
    // `crate::supervisor::lease_win::open`'s own documented contract: an unopenable lease
    // name is reported identically to an opened-but-broken one, never
    // treated as "no lease was ever passed" (that is `None` below).
    // `InheritedFd` (Unix): `producer_pty::parent_lease_fd_broken` does one
    // non-blocking read on the inherited fd — `EAGAIN` means alive,
    // anything else (including a missing fd) means broken, never silently
    // treated as "no lease".
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
    let lease_broken: Option<&dyn Fn() -> bool> =
        config.parent_lease.is_some().then_some(&lease_broken_fn);
    let store =
        VoyageStore::open_for_writing_with_lease(&voyage_root, &config.voyage_id, lease_broken)?;
    Ok(store)
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

fn open_first_segment(
    config: &CapsuleConfig,
    store: &mut VoyageStore,
) -> Result<(FrameCtx, Vec<String>, SegmentWriter)> {
    store.seal_survivor()?;

    let ctx = FrameCtx {
        epoch: store.epoch,
        next_n: 1,
        t0: Instant::now(),
        take_epoch: 0,
        holder: None,
        attached: None,
    };
    // ADR 0041 "Upgrade and version skew" reader-first rollout gate (see
    // `crate::store::rollout`): refuse to open ANY segment for this run if the
    // installed rollback target's reader cannot decode one declaring the
    // EndRun-marker feature. Checked once, before the first segment
    // (rotation reuses the SAME declared set — a run's declared features
    // are its own commitment for its whole life, not renegotiated
    // segment to segment).
    crate::store::rollout::gate(
        &config.rollout_evidence,
        RUN_END_REQUESTED_FEATURE,
    )?;
    // Every segment a step-6 capsule opens declares the EndRun-marker
    // feature UNCONDITIONALLY (ADR 0041 Lifecycle: "a feature cannot be
    // added to an immutable header later and the marker's timing is not
    // knowable in advance").
    let segment_features = vec![RUN_END_REQUESTED_FEATURE.to_string()];
    let w = store.open_segment_with_features(wall_ms(), segment_features.clone())?;
    Ok((ctx, segment_features, w))
}

fn write_take_state(
    store: &VoyageStore,
    ctx: &mut FrameCtx,
    w: &mut SegmentWriter,
    frames_written: &mut u64,
) -> Result<()> {
    // Control preamble — every frame here commits immediately. The step-4
    // "local" take grant is DELETED (ADR 0041 step-5 spec gate): this stops
    // after the null-holder revoke. The first driver ever is a pipe `take`
    // (`Action::CommitTake`, below).
    let prior_take = store.last_take_epoch;
    ctx.take_epoch = prior_take + 1;
    let f = ctx.capsule_frame(
        Class::Lifecycle,
        json!({"kind": "take_state", "take": {"take_epoch": ctx.take_epoch, "holder": null}}),
    );
    w.append(&f, Commit::Immediate)?;
    *frames_written += 1;
    Ok(())
}

fn write_producer_attached(
    config: &CapsuleConfig,
    ctx: &mut FrameCtx,
    w: &mut SegmentWriter,
    frames_written: &mut u64,
) -> Result<()> {
    // producer_attached: the raw-terminal redaction profile, content-hashed
    // — identical to capsule/ (this is a cross-platform semantic, not a
    // Linux one).
    let rules = json!({"input_content": "redacted", "turns": "none"});
    let rules_bytes = serde_json::to_vec(&rules)?;
    let rules_sha = {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(&rules_bytes);
        h.finalize().iter().map(|b| format!("{:02x}", b)).collect::<String>()
    };
    let attached_seq_holder = ctx.capsule_frame(
        Class::ProducerAttached,
        json!({
            "producer_kind": config.producer_kind,
            "version": "raw-pty-1",
            "profile_def": {"id": "raw-terminal-default", "sha256": rules_sha, "rules": rules},
        }),
    );
    let attached_seq = attached_seq_holder.seq;
    w.append(&attached_seq_holder, Commit::Immediate)?;
    *frames_written += 1;
    ctx.attached = Some(attached_seq);
    Ok(())
}

fn write_producer_spawn<P: Producer>(
    config: &CapsuleConfig,
    ctx: &mut FrameCtx,
    w: &mut SegmentWriter,
    frames_written: &mut u64,
) -> Result<()> {
    // producer_spawn commits BEFORE spawn is even attempted — the
    // spawn-failure compensation path (below) depends on this already
    // being on the wire. `P::pre_spawn_detail()` is observed here,
    // independent of spawn's own outcome (see that method's own doc) —
    // any per-producer `SpawnDetail` only available after a SUCCESSFUL
    // spawn would be too late for a failure to fold in. Merged with
    // `argv` rather than nested separately: Windows's own detail shape
    // (`{"spawning_process_was_jobbed": ..., "argv": [...]}`) is
    // unchanged from before this trait existed — `serde_json`'s default
    // (unsorted-input, sorted-output) map means insertion order here
    // never affects the bytes written.
    let mut detail = P::pre_spawn_detail();
    match detail.as_object_mut() {
        Some(map) => {
            map.insert("argv".to_string(), json!(config.argv));
        }
        None => detail = json!({"argv": config.argv}),
    }
    let f = ctx.capsule_frame(Class::Lifecycle, json!({"kind": "producer_spawn", "detail": detail}));
    w.append(&f, Commit::Immediate)?;
    *frames_written += 1;
    Ok(())
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

fn seal_spawn_failure(
    reason: String,
    mut ctx: FrameCtx,
    mut w: SegmentWriter,
    store: &mut VoyageStore,
    mut frames_written: u64,
    mut segments_sealed: u64,
) -> Result<ExitSummary> {
    // Compensation: the producer never ran, but a real spawn
    // attempt was already recorded above — close the run out
    // honestly instead of the Linux capsule's known bare-`?`
    // escape (which seals nothing). No producer exists to tear
    // down; this is the one path that reaches producer_dead
    // without ever having spawned one.
    let f = ctx.capsule_frame(
        Class::Lifecycle,
        json!({"kind": "producer_dead",
               "detail": {"exit_code": null, "spawn_failed": true, "reason": reason}}),
    );
    w.append(&f, Commit::Immediate)?;
    frames_written += 1;
    let digest = w.seal(None)?;
    store.advance_chain(digest);
    segments_sealed += 1;
    Ok(ExitSummary {
        exit_code: None,
        exit_kind: ExitKind::SpawnFailed,
        frames_written,
        segments_sealed,
        handshake_answered: false,
        handshake_suppressed_matches: 0,
        resize_os_calls: 0,
    })
}
