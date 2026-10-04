//! `run`: the capsule writer loop, one producer from spawn to sealed voyage.
use super::*;

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
// unused_assignments: `flush_output!`'s state reset is dead only at its
// FINAL expansion (after the loop) — load-bearing at every other site.
#[allow(unused_assignments)]
pub fn run<P: Producer>(
    config: CapsuleConfig,
    commands: mpsc::Receiver<Command>,
    transport: &mut dyn Transport,
) -> Result<ExitSummary> {
    // A platform this build has no real `self_status` for (any Unix that
    // is neither Linux nor macOS) must refuse BEFORE any durable side
    // effect: without this, such a call would bind the transport, open a
    // segment and commit `take_state`, and only then fail on
    // `Unsupported` — an unsupported call must not change history. On
    // Windows, Linux and macOS the real `self_status` succeeds, so this
    // is a no-op and nothing about any of those paths moves.
    #[cfg(not(windows))]
    let _ = self_status(config.survival)?;
    // Resolve ONCE — the fresh `producer_pty`/`socket_transport` pair on
    // Linux and `producer_conpty`/`pipe_transport` on Windows share this
    // exact ordering (voyage root, then the lease, then the writer fence).
    let voyage_root = crate::fsutil::ensure_container(&config.voyage_root)?;
    if !voyage_root.exists() {
        VoyageStore::bootstrap(&voyage_root, &config.voyage_id, config.retention)?;
    }
    // The lease OPEN itself is deferred to inside this closure, so it
    // happens lazily at the exact point `open_prepared` calls it EXACTLY
    // ONCE -- immediately after the writer fence is acquired, before any
    // other pre-fence-adjacent I/O -- never before. ADR 0043 decision 15:
    // per-platform variant of `ParentLease` — `NamedMutex` (Windows) folds
    // an unopenable/broken name to `true` (broken) here, matching
    // `crate::lease::open`'s own documented contract: an unopenable lease
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
                crate::lease::open(name).map(|c| c.is_broken()).unwrap_or(true)
            }
            #[cfg(unix)]
            Some(ParentLease::InheritedFd(fd)) => crate::producer_pty::parent_lease_fd_broken(*fd),
        }
    };
    let lease_broken: Option<&dyn Fn() -> bool> =
        config.parent_lease.is_some().then_some(&lease_broken_fn);
    let mut store =
        VoyageStore::open_for_writing_with_lease(&voyage_root, &config.voyage_id, lease_broken)?;

    // Finding 7 (round-1) / round-2 finding 4: `shutdown_all` must run
    // before the writer lock releases (`store`'s own drop) on EVERY exit
    // path from this point on, not only the success one -- an RAII guard
    // is the only way to guarantee that regardless of which `?` returns
    // early below. Declared AFTER `store`: Rust drops locals in REVERSE
    // declaration order, so this guard's `Drop` (closing the pipe) runs
    // BEFORE `store`'s (releasing the lock). Constructed HERE, immediately
    // after `store` itself and BEFORE `seal_survivor()?` -- round-2 review
    // caught the guard originally sitting AFTER that call: a failure
    // there would have returned early with the lock already held (via
    // `store`) but the guard never built, releasing the lock with the
    // pipe still live. Every fallible operation from this point on that
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
    let transport = ShutdownGuard(transport);

    // Switch-latency Phase 1 (c): ONE channel for producer output, the
    // reader thread's own death (`ReaderGone`, Codex review PR #227), and
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

    store.seal_survivor()?;

    let mut ctx = FrameCtx {
        epoch: store.epoch,
        next_n: 1,
        t0: Instant::now(),
        take_epoch: 0,
        holder: None,
        attached: None,
    };
    // ADR 0041 "Upgrade and version skew" reader-first rollout gate (see
    // `crate::rollout`): refuse to open ANY segment for this run if the
    // installed rollback target's reader cannot decode one declaring the
    // EndRun-marker feature. Checked once, before the first segment
    // (rotation reuses the SAME declared set — a run's declared features
    // are its own commitment for its whole life, not renegotiated
    // segment to segment).
    crate::rollout::gate(
        &config.rollout_evidence,
        RUN_END_REQUESTED_FEATURE,
    )?;
    // Every segment a step-6 capsule opens declares the EndRun-marker
    // feature UNCONDITIONALLY (ADR 0041 Lifecycle: "a feature cannot be
    // added to an immutable header later and the marker's timing is not
    // knowable in advance").
    let segment_features = vec![RUN_END_REQUESTED_FEATURE.to_string()];
    let mut w = store.open_segment_with_features(wall_ms(), segment_features.clone())?;
    let mut seg_bytes: u64 = 0;
    let mut frames_written: u64 = 0;
    let mut segments_sealed: u64 = 0;

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
    frames_written += 1;

    // The attach protocol: platform-neutral state machine (`attach_proto`);
    // `pid`/`created` are OS values it must never compute itself.
    let mut attach_proto = AttachProto::new(self_status(config.survival)?);
    let mut splitters: HashMap<ConnId, wire::FrameSplitter> = HashMap::new();
    // Finding 11: keyed by (conn, id), not id alone -- a transport's send
    // ids are only ever meaningful scoped to the connection that issued
    // them (a real transport may recycle ids across connections), and
    // every entry for a connection is purged the moment it closes (see
    // `execute_light_actions!`'s `Close` arm), so a canceled write can
    // never leak an entry, nor can a stale/mismatched completion apply a
    // marker meant for a connection that no longer exists.
    let mut pending_sends: HashMap<(ConnId, u64), Option<SentMarker>> = HashMap::new();
    let mut shutdown_requested = false;
    let mut shutdown_reason: Option<String> = None;
    // ADR 0041 EndRun step 2: IRREVOCABLE once true — never unset by
    // anything past this point (a stalled ack, a stopped-reading client,
    // a progress-deadline close, or a lost connection). Distinct from
    // `shutdown_requested`, which governs when TEARDOWN starts (only
    // once the ack ships); this one governs whether the durable marker
    // has already been committed, so a second concurrent `shutdown`
    // request writes no second frame (step 4).
    let mut run_end_latched = false;

    // producer_attached: the raw-terminal redaction profile, content-hashed
    // — identical to capsule.rs (this is a cross-platform semantic, not a
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
    frames_written += 1;
    ctx.attached = Some(attached_seq);

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
    frames_written += 1;

    // Initial geometry validated by the SAME rule a resize is (ADR 0041).
    // An out-of-budget request here is treated exactly like a spawn
    // failure: nothing was ever created, so the same compensation path
    // applies — no separate code path needed for "never even tried".
    let geometry_ok =
        (MIN_COLS..=MAX_COLS).contains(&config.cols) && (MIN_ROWS..=MAX_ROWS).contains(&config.rows);
    let spawn_result = if !geometry_ok {
        Err(format!(
            "initial geometry {}x{} outside the 2x2..512x256 budget",
            config.cols, config.rows
        ))
    } else {
        P::spawn(&config.argv, config.cols, config.rows).map_err(|e| e.to_string())
    };

    let mut producer = match spawn_result {
        Ok(p) => p,
        Err(reason) => {
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
            return Ok(ExitSummary {
                exit_code: None,
                exit_kind: ExitKind::SpawnFailed,
                frames_written,
                segments_sealed,
                handshake_answered: false,
                handshake_suppressed_matches: 0,
                resize_os_calls: 0,
            });
        }
    };
    // N1 (Codex review round 3): the supervisor's own anti-flap counter
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
    let mut parser = vt100_ctt::Parser::new(config.rows, config.cols, CAPSULE_SCROLLBACK_ROWS);
    let mut handshake = HostHandshake::new();
    // ADR 0041's model is ONE host handshake, at startup — answer and
    // record only the first match ever observed; count the rest (the
    // amplification fix, review finding).
    let mut dsr_answered = false;
    let mut handshake_suppressed_matches: u64 = 0;
    let mut resize_os_calls: u64 = 0;

    // The output budget, and the guard that cancels it on ANY exit from
    // this function from this point on — declared as early as the budget
    // itself so an early `?` anywhere below unwinds through it.
    let output_budget = Arc::new(OutputBudget::new());
    let _budget_guard = BudgetCancelGuard(Arc::clone(&output_budget));

    // Three senders share this channel, not one: the reader thread below,
    // `Transport::set_wake`'s callback (registered earlier, sends
    // `TransportActivity`), and this thread's own `ReaderGoneGuard` (sends
    // `ReaderGone` on every exit, including a panic unwind). No bridging
    // threads for input/control though (see the module doc): `commands`
    // is serviced directly, by this loop, from its own separate receiver.
    // `take_output` runs EXACTLY here — before this thread starts, per its
    // own doc — so `producer` itself stays a live, fully-owned binding for
    // every other call this function makes (`input`/`resize`/`wait`/...).
    // `tx`/`output_rx` themselves were created earlier, ahead of
    // `transport.0.bind` (switch-latency Phase 1 (c)) — `tx` is moved
    // into this thread's closure below exactly as before; only its
    // CREATION moved, not its ownership story.
    let mut reader = producer.take_output();
    let reader_handle = {
        let budget = Arc::clone(&output_budget);
        std::thread::spawn(move || {
            // Codex review (PR #227): a drop guard, not another explicit
            // send at the bottom of this closure — the two designed exits
            // already send `Done` and return, but a `read()`/`budget` call
            // panicking partway through would skip any send placed after
            // it. `Drop` runs on every exit, unwind included, which is the
            // one guarantee an ordinary send can't make; see `ReaderGone`'s
            // own doc for why this needs to be a real, matched event
            // rather than relying on the channel's sender count.
            struct ReaderGoneGuard(mpsc::Sender<ReaderEvent>);
            impl Drop for ReaderGoneGuard {
                fn drop(&mut self) {
                    let _ = self.0.send(ReaderEvent::ReaderGone);
                }
            }
            let _reader_gone_guard = ReaderGoneGuard(tx.clone());
            let mut buf = [0u8; READ_CHUNK];
            loop {
                if !budget.reserve(READ_CHUNK as u64) {
                    // Cancelled: `run` is already exiting some other way.
                    // Nothing left to report; just stop.
                    return;
                }
                match reader.read(&mut buf) {
                    Ok(0) => {
                        budget.release(READ_CHUNK as u64);
                        let _ = tx.send(ReaderEvent::Done(Ok(())));
                        return;
                    }
                    // Review round 2 (R4): a signal-interrupted read is not
                    // an end of stream on ANY platform -- the ConPTY
                    // producer never actually produces this (Windows has
                    // no equivalent signal-delivery-during-read
                    // interruption for a named pipe read), but the Unix
                    // pty producer's plain `File` can, any time the
                    // reading thread receives a signal (this crate's own
                    // `Drop`-time `killpg`/`waitpid` and the reap-bound
                    // polling elsewhere don't target this thread, but an
                    // operator/OS signal targeting the whole process
                    // would). Release the reservation and retry the SAME
                    // read rather than treating it as terminal -- the loop
                    // re-reserves at its own top.
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                        budget.release(READ_CHUNK as u64);
                        continue;
                    }
                    Err(e) => {
                        budget.release(READ_CHUNK as u64);
                        let _ = tx.send(ReaderEvent::Done(Err(e)));
                        return;
                    }
                    Ok(n) => {
                        let n = n as u64;
                        if n < READ_CHUNK as u64 {
                            budget.release(READ_CHUNK as u64 - n);
                        }
                        if tx.send(ReaderEvent::Output(buf[..n as usize].to_vec())).is_err() {
                            budget.release(n);
                            return;
                        }
                    }
                }
            }
        })
    };

    let mut pending_output: Vec<u8> = Vec::new();
    let mut pending_bytes: usize = 0;
    let mut last_commit = Instant::now();
    let mut last_fsync = Instant::now();
    let mut last_output = Instant::now();
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
    // Round-2 review, finding 10 (the fairness claim below was overstated):
    // `pace_output!` sleeps 1 ms per `GROUP_COMMIT_BYTES` of output
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
    // `service_transport_events!`/`tick` already run once per iteration,
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
    let mut bytes_since_yield: usize = 0;

    // THIS module decides nothing; `attach_proto::AttachProto` does (see
    // its module doc). `execute_light_actions!` runs the action kinds that
    // `output_committed`/`ground_reached`/`checkpoint_ready` can ever
    // produce (proven by `attach_proto`'s own implementation: never
    // `CommitTake`/`ForwardInput`/`ApplyResize`/`Shutdown`) -- kept SEPARATE
    // from the full `execute_actions!` below rather than one macro calling
    // itself, because `flush_output!` needs to run actions too, and
    // `flush_output!` is itself called FROM `execute_actions!`'s
    // `CommitTake`/`ApplyResize` arms: a macro invoking itself through that
    // path is not runtime recursion (which would be fine) but INFINITE
    // COMPILE-TIME macro expansion (`recursion limit reached`, hit and
    // fixed while building this unit) -- every match arm is expanded
    // unconditionally at compile time, `flush_output!`'s body included,
    // regardless of which arm ever actually runs. Splitting the acyclic
    // subset out breaks the cycle: `execute_light_actions!` calls nothing
    // else here; `flush_output!` calls only `execute_light_actions!`;
    // `execute_actions!` calls `flush_output!`, `maybe_rotate!`, and (for
    // its own light-kind actions) `execute_light_actions!` -- all strictly
    // "downward", never back.
    macro_rules! execute_light_actions {
        ($seed:expr) => {{
            let mut queue: VecDeque<AttachAction> = VecDeque::from($seed);
            while let Some(action) = queue.pop_front() {
                match action {
                    AttachAction::Send { conn, frame_bytes, marker } => {
                        let id = transport.0.send(conn, frame_bytes);
                        // Round-2 review, finding 7: the Transport contract
                        // (this trait's own doc) requires every outstanding
                        // (conn, id) to be unique -- a reused id before its
                        // predecessor's completion is reported would
                        // silently resolve the WRONG marker for a later
                        // `Sent`. A transport that violates this has a bug
                        // in it, not something this loop can route around.
                        let prior = pending_sends.insert((conn, id), marker);
                        assert!(
                            prior.is_none(),
                            "Transport::send returned (conn={conn:?}, id={id}) while a previous send with the \
                             SAME id was still outstanding -- violates the unique-outstanding-id contract"
                        );
                    }
                    AttachAction::Close(conn) => {
                        transport.0.close(conn);
                        splitters.remove(&conn);
                        // Finding 11: purge every pending send this
                        // connection still had outstanding -- a canceled
                        // write's completion, if the transport ever
                        // reported one anyway, must find nothing to apply
                        // a marker to.
                        pending_sends.retain(|&(c, _), _| c != conn);
                        queue.extend(attach_proto.connection_closed(conn, Instant::now()));
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
                        let legacy = attach_proto.negotiated_proto(conn) == wire::ATTACH_PROTO_V1;
                        let bytes = if legacy {
                            parser.screen().checkpoint_at_version(LEGACY_CHECKPOINT_VERSION)
                        } else {
                            parser.screen().checkpoint()
                        }
                        .expect(
                            "geometry is bounded to 2x2..512x256, always representable at that range (ADR 0041)",
                        );
                        queue.extend(attach_proto.checkpoint_ready(conn, bytes, Instant::now()));
                    }
                    other => unreachable!(
                        "execute_light_actions!: {other:?} is not one output_committed/ground_reached/checkpoint_ready can produce"
                    ),
                }
            }
        }};
    }

    macro_rules! flush_output {
        ($w:expr) => {
            if pending_bytes > 0 {
                $w.commit()?; // the watermark: fsync BEFORE anything is published
                last_fsync = Instant::now();
                execute_light_actions!(attach_proto.output_committed(&pending_output, Instant::now()));
                pending_output.clear();
                pending_bytes = 0;
            } else if $w.has_unsynced() {
                // A Buffered input-WAL record with no output behind it: commit it
                // by the next group-commit check (ADR 0039 Durability invariants);
                // nothing is published, so no `output_committed`.
                $w.commit()?;
                last_fsync = Instant::now();
            }
            last_commit = Instant::now();
            // ADR 0041: attach is GROUND-GATED; the watermark barrier
            // (force pending commit -> publish to EXISTING subscribers ->
            // checkpoint -> subscribe) is exactly this ordering -- publish
            // above already ran, so a ground boundary found HERE is the
            // single loop step the barrier requires.
            if parser.is_ground() {
                execute_light_actions!(attach_proto.ground_reached(Instant::now()));
            }
        };
    }

    macro_rules! maybe_rotate {
        ($w:ident) => {
            if seg_bytes >= SEGMENT_MAX_BYTES {
                flush_output!($w);
                let digest = $w.seal(None)?;
                store.advance_chain(digest);
                segments_sealed += 1;
                $w = store.open_segment_with_features(wall_ms(), segment_features.clone())?;
                seg_bytes = 0;
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
            bytes_since_yield += $bytes.len();
            handle_output!($bytes);
            if bytes_since_yield >= GROUP_COMMIT_BYTES {
                std::thread::sleep(Duration::from_millis(1));
                bytes_since_yield = 0;
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
            if attach_proto.ground_gate_pending() {
                flush_output!(w);
            }
        };
    }

    // The full action set -- everything `execute_light_actions!` handles,
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
                        execute_light_actions!(vec![light]);
                    }
                    AttachAction::CommitTake { conn, controller_id, request_id } => {
                        flush_output!(w);
                        ctx.take_epoch += 1;
                        ctx.holder = Some(controller_id.clone());
                        let f = ctx.capsule_frame(
                            Class::Lifecycle,
                            json!({"kind": "take_state",
                                   "take": {"take_epoch": ctx.take_epoch, "holder": controller_id.clone()}}),
                        );
                        w.append(&f, Commit::Immediate)?;
                        frames_written += 1;
                        queue.extend(attach_proto.take_committed(conn, controller_id, ctx.take_epoch, request_id, Instant::now()));
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
                            &mut ctx,
                            &mut w,
                            &mut store,
                            producer.input(),
                            &mut frames_written,
                            &controller_id,
                            take_epoch,
                            idem_key,
                            &payload,
                            connection_authorized,
                        )?;
                        maybe_rotate!(w);
                        queue.extend(attach_proto.input_outcome(conn, outcome, request_id, Instant::now()));
                    }
                    AttachAction::ApplyResize { conn, cols, rows, request_id } => {
                        // ADR 0041: "resize (driver-only) routes into the
                        // step-4 exchange unchanged" -- same ordered
                        // request -> one ResizePseudoConsole call (skipped
                        // if out of budget) -> parser/geometry updated
                        // only on success -> outcome shape step 4 already
                        // built, now reachable from the wire too.
                        flush_output!(w);
                        let req = ctx.current_controller_frame(
                            Class::ControlExchange,
                            json!({"phase": "request", "kind_ns": "conpty/resize",
                                   "to": {"kind": "producer"}, "body": {"cols": cols, "rows": rows}}),
                        );
                        let req_seq = req.seq;
                        w.append(&req, Commit::Immediate)?;
                        frames_written += 1;
                        let in_budget =
                            (MIN_COLS..=MAX_COLS).contains(&cols) && (MIN_ROWS..=MAX_ROWS).contains(&rows);
                        let ok = if !in_budget {
                            false
                        } else {
                            resize_os_calls += 1;
                            match producer.resize(cols, rows) {
                                Ok(()) => {
                                    parser.screen_mut().set_size(rows, cols);
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
                        let out = ctx.current_controller_frame(
                            Class::ControlExchange,
                            json!({"phase": "outcome", "kind_ns": "conpty/resize", "scope": "pty",
                                   "target": format!("{}:{}", req_seq.epoch, req_seq.n), "body": outcome_body}),
                        );
                        w.append(&out, Commit::Immediate)?;
                        frames_written += 1;
                        maybe_rotate!(w);
                        queue.extend(attach_proto.resize_outcome(conn, ok, cols, rows, request_id, Instant::now()));
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
                        shutdown_reason.get_or_insert_with(|| reason.clone());
                        commit_run_end_marker(&mut ctx, &mut w, &mut frames_written, &mut run_end_latched, reason)?;
                    }
                    AttachAction::Shutdown { reason } => {
                        shutdown_requested = true;
                        shutdown_reason.get_or_insert(reason);
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
                let Some(ev) = transport.0.try_recv_event() else { break };
                match ev {
                    TransportEvent::ConnectionOpened(conn) => {
                        splitters.insert(conn, wire::FrameSplitter::new());
                        execute_actions!(attach_proto.connection_opened(conn, Instant::now()));
                    }
                    TransportEvent::Bytes(conn, bytes) => {
                        let Some(splitter) = splitters.get_mut(&conn) else { continue };
                        let (frames, err) = splitter.feed(&bytes);
                        for f in frames {
                            execute_actions!(attach_proto.frame(conn, f, Instant::now()));
                        }
                        if err.is_some() {
                            transport.0.close(conn);
                            splitters.remove(&conn);
                            pending_sends.retain(|&(c, _), _| c != conn); // finding 11
                            execute_actions!(attach_proto.connection_closed(conn, Instant::now()));
                        }
                    }
                    TransportEvent::ConnectionClosed(conn) => {
                        splitters.remove(&conn);
                        pending_sends.retain(|&(c, _), _| c != conn); // finding 11
                        execute_actions!(attach_proto.connection_closed(conn, Instant::now()));
                    }
                    TransportEvent::Sent(conn, id) => {
                        match pending_sends.remove(&(conn, id)) {
                            Some(marker) => execute_actions!(attach_proto.sent(conn, marker, Instant::now())),
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
                                !splitters.contains_key(&conn),
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
                        shutdown_requested = true;
                        shutdown_reason = Some("transport-accept-failed".to_string());
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
    // runs there) delegates to `execute_light_actions!`; `RunEndRequested`/
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
                        execute_light_actions!(vec![light]);
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
                        shutdown_reason.get_or_insert_with(|| reason.clone());
                        commit_run_end_marker(&mut ctx, &mut w, &mut frames_written, &mut run_end_latched, reason)?;
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
                        shutdown_reason.get_or_insert(reason);
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
                let Some(ev) = transport.0.try_recv_event() else { break };
                match ev {
                    TransportEvent::ConnectionOpened(conn) => {
                        splitters.insert(conn, wire::FrameSplitter::new());
                        execute_teardown_actions!(attach_proto.connection_opened(conn, Instant::now()));
                    }
                    TransportEvent::Bytes(conn, bytes) => {
                        let Some(splitter) = splitters.get_mut(&conn) else { continue };
                        let (frames, err) = splitter.feed(&bytes);
                        for f in frames {
                            execute_teardown_actions!(attach_proto.frame(conn, f, Instant::now()));
                        }
                        if err.is_some() {
                            transport.0.close(conn);
                            splitters.remove(&conn);
                            pending_sends.retain(|&(c, _), _| c != conn);
                            execute_teardown_actions!(attach_proto.connection_closed(conn, Instant::now()));
                        }
                    }
                    TransportEvent::ConnectionClosed(conn) => {
                        splitters.remove(&conn);
                        pending_sends.retain(|&(c, _), _| c != conn);
                        execute_teardown_actions!(attach_proto.connection_closed(conn, Instant::now()));
                    }
                    TransportEvent::Sent(conn, id) => {
                        match pending_sends.remove(&(conn, id)) {
                            Some(marker) => execute_teardown_actions!(attach_proto.sent(conn, marker, Instant::now())),
                            // Finding 7, same reasoning as the main loop's
                            // identical arm: tolerated only for a
                            // connection already closed.
                            None => assert!(
                                !splitters.contains_key(&conn),
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
                        shutdown_reason.get_or_insert_with(|| "transport-accept-failed".to_string());
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
                let Some(ev) = transport.0.try_recv_event() else { break };
                match ev {
                    TransportEvent::ConnectionOpened(conn) => {
                        // Never admitted: no splitter, no `attach_proto`
                        // event, just closed.
                        transport.0.close(conn);
                    }
                    TransportEvent::Bytes(conn, _bytes) => {
                        // A connection admitted during ORDINARY teardown
                        // (before the grace began) sending more bytes now:
                        // still no new admission -- close it, purging
                        // whatever this loop already tracked for it.
                        transport.0.close(conn);
                        splitters.remove(&conn);
                        pending_sends.retain(|&(c, _), _| c != conn);
                        execute_teardown_actions!(attach_proto.connection_closed(conn, Instant::now()));
                    }
                    TransportEvent::ConnectionClosed(conn) => {
                        splitters.remove(&conn);
                        pending_sends.retain(|&(c, _), _| c != conn);
                        execute_teardown_actions!(attach_proto.connection_closed(conn, Instant::now()));
                    }
                    TransportEvent::Sent(conn, id) => {
                        match pending_sends.remove(&(conn, id)) {
                            Some(marker) => execute_teardown_actions!(attach_proto.sent(conn, marker, Instant::now())),
                            None => assert!(
                                !splitters.contains_key(&conn),
                                "Transport reported Sent({conn:?}, {id}) for an ACTIVE connection with no \
                                 matching outstanding send"
                            ),
                        }
                    }
                    TransportEvent::TransportFatal(detail) => {
                        eprintln!(
                            "sot-capsule: transport reported a terminal failure during the shutdown-ack grace: {detail}"
                        );
                        shutdown_reason.get_or_insert_with(|| "transport-accept-failed".to_string());
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
            parser.process(&bytes);

            use base64_engine::encode_b64;
            let f = ctx.producer_frame(json!({"bytes_b64": encode_b64(&bytes)}));
            w.append(&f, Commit::Buffered)?;
            frames_written += 1;
            seg_bytes += bytes.len() as u64 + 128;
            output_budget.release(bytes.len() as u64);
            pending_output.extend_from_slice(&bytes);
            pending_bytes += bytes.len();
            if pending_bytes >= GROUP_COMMIT_BYTES {
                flush_output!(w);
            }

            let matches = handshake.feed(&bytes);
            if matches > 0 {
                if !dsr_answered {
                    dsr_answered = true;
                    // Query exchange, ADR 0041's own phrase and shape:
                    // request -> response (only on a successful write) ->
                    // outcome (always, reflecting whether it was).
                    let req = ctx.capsule_frame(
                        Class::ControlExchange,
                        json!({"phase": "request", "kind_ns": "conpty/host-handshake",
                               "to": {"kind": "producer"}, "body": {"query": "da1"}}),
                    );
                    let req_seq = req.seq;
                    w.append(&req, Commit::Immediate)?;
                    frames_written += 1;

                    let write_result = producer.input().write_all(host_handshake::DA1_REPLY);
                    if write_result.is_ok() {
                        let mut resp = ctx.capsule_frame(
                            Class::ControlExchange,
                            json!({"phase": "response", "kind_ns": "conpty/host-handshake",
                                   "body": {"query": "da1"}}),
                        );
                        resp.refs = vec![FrameRef { kind: RefKind::RespondsTo, frame: req_seq }];
                        w.append(&resp, Commit::Immediate)?;
                        frames_written += 1;
                    }
                    let outcome_body = match &write_result {
                        Ok(()) => json!({"disposition": "ok"}),
                        Err(e) => json!({"disposition": "failed", "reason": e.to_string()}),
                    };
                    let out = ctx.capsule_frame(
                        Class::ControlExchange,
                        json!({"phase": "outcome", "kind_ns": "conpty/host-handshake", "scope": "pty",
                               "target": format!("{}:{}", req_seq.epoch, req_seq.n), "body": outcome_body}),
                    );
                    w.append(&out, Commit::Immediate)?;
                    frames_written += 1;

                    // Any FURTHER matches in this SAME chunk are already
                    // "later" than the one just answered.
                    handshake_suppressed_matches += (matches - 1) as u64;
                } else {
                    handshake_suppressed_matches += matches as u64;
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
    // Set by the ONE rule below (both the main loop's arm and teardown
    // Phase A's identical one): a terminal reader event that arrived before
    // `close_output_side` and that a confirmed producer exit explained. Stays
    // `None` on Linux and on Windows by those platforms' own contracts -- see
    // the arm itself -- and its presence in `producer_dead.detail` is how the
    // one admitted case is RECORDED rather than forgiven.
    let mut output_ended_early: Option<String> = None;

    let exit_kind = 'main: loop {
        if producer.wait(Duration::ZERO)? {
            break 'main ExitKind::ProducerExited;
        }
        service_transport_events!();
        execute_actions!(attach_proto.tick(Instant::now()));
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
        if shutdown_requested || run_end_latched {
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
                shutdown_reason.get_or_insert_with(|| "operator_kill".to_string());
                commit_run_end_marker(
                    &mut ctx,
                    &mut w,
                    &mut frames_written,
                    &mut run_end_latched,
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
        match output_rx.recv_timeout(output_wait(
            pending_bytes,
            last_commit.elapsed(),
            last_output.elapsed(),
            last_fsync.elapsed(),
        )) {
            Ok(ReaderEvent::Output(bytes)) => {
                last_output = Instant::now();
                pace_output!(bytes);
                maybe_rotate!(w);
            }
            Ok(ReaderEvent::TransportActivity) => {
                wake_pending.store(false, Ordering::Release);
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
                if !producer.wait(READER_END_EXIT_GRACE)? {
                    return Err(Error::State(format!(
                        "capsule_win: reader reached its terminal state before close_pty was ever \
                         called, and the producer was still alive {READER_END_EXIT_GRACE:?} later: {result:?}"
                    )));
                }
                output_ended_early = Some(format!("{result:?}"));
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
        if should_flush_output(last_commit.elapsed(), pending_bytes, last_output.elapsed(), last_fsync.elapsed()) {
            flush_output!(w);
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
    flush_output!(w);

    // Producer-bound admission (take/input/resize) is revoked from here on
    // (finding 7) — but mgmt (probe/status/shutdown) and Sent completions
    // keep being serviced through BOTH phases below, via
    // `service_transport_events_teardown!`, until the pipe closes (see that
    // macro's own doc for why, and `execute_teardown_actions!` for the
    // reduced action set this implies).
    attach_proto.begin_teardown();

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
    producer.terminate_domain()?;
    let reap_deadline = Instant::now() + TEARDOWN_REAP_TIMEOUT;
    loop {
        service_transport_events_teardown!();
        execute_teardown_actions!(attach_proto.tick(Instant::now()));
        eager_ground_check!();
        if producer.domain_is_empty()? {
            break;
        }
        if Instant::now() >= reap_deadline {
            return Err(Error::State(
                "capsule_win: job did not reap within the teardown timeout".into(),
            ));
        }
        match output_rx.recv_timeout(TEARDOWN_REAP_POLL) {
            Ok(ReaderEvent::Output(bytes)) => {
                pace_output!(bytes);
                maybe_rotate!(w);
            }
            Ok(ReaderEvent::TransportActivity) => {
                // Switch-latency Phase 1 (c): same wake, same channel, as
                // the main loop's own arm -- mgmt/`Sent` traffic keeps
                // being serviced through teardown (finding 7), so it gets
                // the same early wake here rather than waiting out
                // `TEARDOWN_REAP_POLL`. `service_transport_events_teardown!`
                // at this loop's own top does the actual draining.
                wake_pending.store(false, Ordering::Release);
            }
            Ok(ReaderEvent::Done(result)) => {
                // The same rule as the main loop's identical arm, and for
                // the same reason: nothing has called close_pty() yet, so
                // this cannot be an ordinary end of the drain -- it is the
                // producer's own exit revoking the pty, or it is a defect.
                // The difference here is only that this loop has a reap to
                // finish, so it records and keeps polling.
                if !producer.wait(READER_END_EXIT_GRACE)? {
                    return Err(Error::State(format!(
                        "capsule_win: reader reached its terminal state during reap with the producer \
                         still alive {READER_END_EXIT_GRACE:?} later: {result:?}"
                    )));
                }
                output_ended_early = Some(format!("{result:?}"));
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
                if output_ended_early.is_none() {
                    return Err(Error::State(
                        "capsule_win: the reader thread ended without a terminal Done event during reap".into(),
                    ));
                }
            }
        }
    }
    flush_output!(w);

    // Phase B: close the pseudoconsole on a DEDICATED thread so THIS loop
    // can keep draining `output_rx` (feeding `handle_output!`, answering
    // the handshake) CONCURRENTLY with the close — the documented call
    // pattern ("reader already draining, THEN call this") applied
    // literally: draining must never itself pause to make the call. Both a
    // graceful EOF and a broken-pipe error are the ORDINARY, expected end
    // of this drain (the close is what produces them) — unlike Phase A's
    // identical-looking check, neither is an anomaly here.
    let closer_handle = producer.close_output_side();
    // The close itself is UNCONDITIONAL -- it drops the held slave (a
    // real close(2), still owed on a revoked fd), keeps `Drop`
    // idempotent, and yields the `closer_handle` the aggregate join
    // below needs. Only the DRAIN is guarded: when the output side
    // already reached its terminal state before this point (see the
    // arms above), there is no EOF left for this loop to wait out, and
    // waiting for one would burn `TEARDOWN_DRAIN_TIMEOUT` and then fail
    // a run that is in fact complete.
    if output_ended_early.is_none() {
        let drain_deadline = Instant::now() + TEARDOWN_DRAIN_TIMEOUT;
        loop {
            service_transport_events_teardown!();
            execute_teardown_actions!(attach_proto.tick(Instant::now()));
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
            match output_rx.recv_timeout(TEARDOWN_DRAIN_POLL) {
                Ok(ReaderEvent::Output(bytes)) => {
                    pace_output!(bytes);
                    maybe_rotate!(w);
                }
                Ok(ReaderEvent::TransportActivity) => {
                    // Switch-latency Phase 1 (c): same wake as both other
                    // sites -- see the main loop's own arm.
                    wake_pending.store(false, Ordering::Release);
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
                    execute_teardown_actions!(attach_proto.tick(Instant::now()));
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
        execute_teardown_actions!(attach_proto.tick(Instant::now()));
    }
    flush_output!(w);

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
    while pending_sends
        .values()
        .any(|m| matches!(m, Some(SentMarker::ShutdownAck { .. })))
        && Instant::now() < shutdown_ack_deadline
    {
        drain_pending_sends_only!();
        execute_teardown_actions!(attach_proto.tick(Instant::now()));
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
    let transport_ok = transport.0.shutdown_all(teardown_deadline);
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
    if !producer.wait(Duration::from_secs(5))? {
        return Err(Error::State(
            "capsule_win: producer did not signal after its domain reaped to empty".into(),
        ));
    }
    let exit_status = producer.exit_status_after_confirmed_exit()?;

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
    if let Some(reason) = &shutdown_reason {
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
    if let Some(how) = &output_ended_early {
        detail["output_ended_early"] = json!(how);
    }
    let f = ctx.capsule_frame(Class::Lifecycle, json!({"kind": "producer_dead", "detail": detail}));
    w.append(&f, Commit::Immediate)?;
    frames_written += 1;

    let digest = w.seal(None)?;
    store.advance_chain(digest);
    segments_sealed += 1;

    Ok(ExitSummary {
        exit_code: Some(exit_status),
        exit_kind,
        frames_written,
        segments_sealed,
        handshake_answered: dsr_answered,
        handshake_suppressed_matches,
        resize_os_calls,
    })
}
