//! Capsule tests for teardown: the durable marker, concurrent shutdowns, every exit path, expiry and ack grace.

use super::*;

/// Test 11 (REWRITTEN, Codex round-1 Blocker 1 discharge): the durable
/// MARKER — not the ack — drives teardown. "Ack completion only
/// ACCELERATES teardown" (ADR 0041 EndRun step 2): a stalled ack, a
/// client that stops reading, a progress-deadline close, or a lost
/// connection cannot unlatch it. Proven by holding the `shutdown_ok`
/// ack's physical-send completion and NEVER RELEASING IT — confirming
/// the marker is already durable on the still-open leg while the ack is
/// held, then confirming `run` still completes and seals within a
/// bounded time regardless (the ack-grace window still expires
/// normally, exactly as `shutdown_ack_grace_expires_and_teardown_still_
/// completes` proves for a Kill-driven teardown; this is the SAME
/// mechanism with the wire request itself as the ONLY driver, no
/// separate cause). The reason string still lands in `producer_dead`'s
/// detail — recorded from the marker's own commit (see
/// `commit_run_end_marker`'s call sites), never from the ack's
/// completion, which here never happens at all.
///
/// The ORIGINAL version of this test asserted the opposite
/// (`!handle.is_finished()` while the ack was held, released later) —
/// that encoded the design Blocker 1 identifies as wrong: a stalled ack
/// must never be able to leave a durable marker coexisting with a shell
/// running on.
#[test]
fn teardown_completes_even_when_the_shutdown_ack_is_never_delivered() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()]; // stays open until EndRun
    let cfg = config(dir.path(), "neverack1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let transport = TestTransport::new();
    let (_tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    const MGMT: ConnId = 1;
    transport.open(MGMT);
    transport.set_hold_for(MGMT, true); // held FOREVER -- never released below
    transport.feed(MGMT, frame::mgmt_shutdown("never-delivered-ack"));

    // The ack's bytes are constructed and queued...
    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("mgmt shutdown_ok queued", MGMT, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ShutdownOk)).then_some(())
    });

    // ...and the marker is ALREADY durable on the still-open leg, even
    // though that ack's physical completion will NEVER be reported.
    let seg_dir = root.join("seg");
    let marker_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if leg_carries_run_end_marker(&seg_dir, "neverack1", 1).unwrap_or(false) {
            break;
        }
        assert!(Instant::now() < marker_deadline, "marker never became visible on the still-open leg");
        std::thread::sleep(Duration::from_millis(20));
    }

    // `transport.release_held()` is deliberately NEVER called -- the
    // whole point is that teardown does not need it.
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect(
            "run did not complete even though the durable marker should drive teardown \
             regardless of the never-delivered ack",
        )
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    verify_voyage(&root, "neverack1").unwrap();
    assert!(leg_carries_run_end_marker(&seg_dir, "neverack1", 1).unwrap());

    let frames = sealed_frames(&root, "neverack1");
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["reason"], "never-delivered-ack");
}

/// ADR 0041 step 6 U1b, acceptance matrix "the marker is the acceptance
/// barrier": the marker is durable on the STILL-OPEN leg (before `run`
/// has sealed anything) essentially as soon as the request is processed
/// — and, distinctly from `teardown_completes_even_when_the_shutdown_
/// ack_is_never_delivered` above (which proves teardown does NOT need
/// the ack), this test proves the ack is still a working COURTESY when
/// nothing prevents it: released promptly, its bytes still show up as a
/// well-formed `ShutdownOk` reply on the same connection. "Ack completion
/// only accelerates teardown" cuts both ways — it must never be
/// REQUIRED, but it must still WORK.
#[test]
fn marker_is_durable_before_the_ack_completes_and_the_ack_still_works() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()]; // stays open until EndRun
    let cfg = config(dir.path(), "markerstall1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let transport = TestTransport::new();
    let (_tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    const MGMT: ConnId = 1;
    transport.open(MGMT);
    transport.set_hold_for(MGMT, true); // held BEFORE the request that matters
    transport.feed(MGMT, frame::mgmt_shutdown("marker-before-ack"));

    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("mgmt shutdown_ok queued", MGMT, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ShutdownOk)).then_some(())
    });

    // The ack is QUEUED but its physical-write completion is HELD -- the
    // marker must already be durable regardless. Nothing is sealed yet
    // (teardown may already be under way), so poll the STILL-OPEN leg
    // directly.
    let seg_dir = root.join("seg");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if leg_carries_run_end_marker(&seg_dir, "markerstall1", 1).unwrap_or(false) {
            break;
        }
        assert!(Instant::now() < deadline, "marker never became visible on the still-open leg");
        std::thread::sleep(Duration::from_millis(20));
    }

    transport.release_held();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return after the ack was released")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    verify_voyage(&root, "markerstall1").unwrap();
    assert!(leg_carries_run_end_marker(&seg_dir, "markerstall1", 1).unwrap());
    // The courtesy still worked: the EARLIER `wait_for` already decoded a
    // well-formed `ShutdownOk` reply queued for this connection, and
    // `release_held` + the successful `wait_for_join` above prove its
    // physical-send completion was processed normally through teardown --
    // the ack is not required, but it is not broken either.
}

/// ADR 0041 step 6 U1b, acceptance matrix "the marker is the acceptance
/// barrier", step 4: two concurrent callers get ONE marker and TWO acks.
/// Real concurrency at the wire level (two mgmt connections, both
/// requesting before either's ack physically completes) — the end-to-end
/// proof that the WIRING (`attach_proto`'s `Action::RunEndRequested`
/// ordering, `execute_actions`'s dispatch) actually delivers the
/// guarantee `commit_run_end_marker`'s own unit tests already prove in
/// isolation against the pure function.
#[test]
fn two_concurrent_shutdown_requests_write_one_marker_and_ack_both() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()];
    let cfg = config(dir.path(), "concurrentshutdown1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let transport = TestTransport::new();
    let (_tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    const MGMT_A: ConnId = 1;
    const MGMT_B: ConnId = 2;
    transport.open(MGMT_A);
    transport.open(MGMT_B);
    // Hold BOTH acks until both requests are already queued -- a genuine
    // race between the two callers, not two sequential round trips.
    transport.set_hold_for(MGMT_A, true);
    transport.set_hold_for(MGMT_B, true);
    transport.feed(MGMT_A, frame::mgmt_shutdown("caller-a"));
    transport.feed(MGMT_B, frame::mgmt_shutdown("caller-b"));

    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("A shutdown_ok queued", MGMT_A, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ShutdownOk)).then_some(())
    });
    watcher.wait_for("B shutdown_ok queued", MGMT_B, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ShutdownOk)).then_some(())
    });

    transport.release_held();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return after both acks were released")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    verify_voyage(&root, "concurrentshutdown1").unwrap();

    let frames = sealed_frames(&root, "concurrentshutdown1");
    let marker_count = frames
        .iter()
        .filter(|f| {
            f.class == Class::Lifecycle
                && f.payload.as_ref().and_then(|p| p.get("kind")).and_then(|k| k.as_str())
                    == Some("run_end_requested")
        })
        .count();
    assert_eq!(marker_count, 1, "two concurrent callers must write exactly one marker");
}

/// Test 12 (finding 7, Codex review rework): `Transport::shutdown_all` is
/// actually invoked before `run` returns, on every exit path -- the one
/// piece of the teardown rework that is new wiring, not a restatement of
/// something the pure-logic `AttachProto` tests already cover. The reduced
/// legal action set teardown enforces (mgmt served, producer-bound
/// admission revoked, no lockstep leak from an ignored request) is proven
/// exhaustively and race-free at that level already
/// (`attach_proto::teardown_ignores_producer_bound_requests_but_not_mgmt_or_attach`)
/// — reproving it here against a real ConPTY would only buy a race between
/// the test thread's writes and whichever loop iteration observes them
/// first, without adding coverage `execute_teardown_actions`'s own
/// `unreachable!` arms don't already give at compile time.
///
/// U1a Codex round-1, minor cluster: asserts the exact count (2), not
/// merely "at least once" -- `run` now calls `shutdown_all` explicitly
/// once the (zero-iteration, on these paths) ack grace resolves, AND
/// `ShutdownGuard::drop` calls it again unconditionally afterward. Proving
/// the count is exactly 2 is what actually exercises `Transport::
/// shutdown_all`'s documented idempotent contract, rather than merely
/// trusting it.
#[test]
fn shutdown_all_is_called_before_run_returns_on_every_exit_path() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();

    // Path 1: a natural producer exit.
    {
        let argv = shell_command("exit 0");
        let cfg = config(dir.path(), "shutdownall1", argv, 80, 25);
        let transport = TestTransport::new();
        let (_tx, rx) = mpsc::channel();
        let mut run_transport = transport.clone();
        let summary = capsule::run::<P>(cfg, rx, &mut run_transport).unwrap();
        assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
        assert_eq!(
            transport.shutdown_all_call_count(),
            2,
            "shutdown_all must be called exactly twice (the explicit ack-grace call, then ShutdownGuard::drop) even on a natural producer exit"
        );
    }

    // Path 2: a requested kill, with an attached connection still open --
    // proving `shutdown_all` runs even when the pipe has real state on it,
    // not only in the no-connections-ever-opened case above.
    {
        let argv = vec![SHELL_ARGV.to_string()]; // stays open until killed
        let cfg = config(dir.path(), "shutdownall2", argv, 80, 25);
        let transport = TestTransport::new();
        let (tx, rx) = mpsc::channel();
        let run_transport = transport.clone();
        let handle = std::thread::spawn(move || {
            let mut t = run_transport;
            capsule::run::<P>(cfg, rx, &mut t)
        });

        const CONN: ConnId = 1;
        transport.open(CONN);
        transport.feed(CONN, frame::hello());
        let mut watcher = FrameWatcher::new(&transport);
        watcher.wait_for("conn hello_ok", CONN, Duration::from_secs(10), |f| {
            matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
        });
        transport.feed(CONN, frame::attach("watcher"));
        watcher.collect_checkpoint("conn checkpoint", CONN, Duration::from_secs(10));

        tx.send(Command::Kill).unwrap();
        let summary = wait_for_join(handle, Duration::from_secs(30))
            .expect("run did not return within the teardown bound")
            .unwrap();
        assert_eq!(summary.exit_kind, ExitKind::Requested);
        assert_eq!(
            transport.shutdown_all_call_count(),
            2,
            "shutdown_all must be called exactly twice on a requested kill too"
        );
    }
}

/// Codex round-1 Blocker 3 discharge: aggregate-teardown expiry is
/// TERMINAL, not a "loud but successful" return. `run` must not seal the
/// voyage or report `Ok(ExitSummary)` past a teardown whose transport
/// could not prove every worker stopped within the shared deadline — the
/// writer fence (`store`, dropped via this same early return) is the
/// only thing that may release past it. Simulated via `TestTransport::
/// force_shutdown_expiry` (a real Windows stalled-worker scenario is
/// proven separately, at the transport level, by `lane/pipe_win/`'s own
/// `stalled_worker_does_not_block_teardown_of_healthy_connections` and
/// the pure `join_within` expiry tests) — this test's job is specifically
/// `capsule::run`'s OWN reaction to that report.
#[test]
fn aggregate_teardown_expiry_is_terminal_not_a_silent_seal() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = shell_command("exit 0");
    let cfg = config(dir.path(), "expiryterminal1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let transport = TestTransport::new();
    transport.force_shutdown_expiry();
    let (_tx, rx) = mpsc::channel();
    let mut run_transport = transport.clone();
    let err = capsule::run::<P>(cfg, rx, &mut run_transport).unwrap_err();
    assert!(
        format!("{err}").contains("aggregate teardown"),
        "expected a named aggregate-teardown failure, got: {err}"
    );
    // No seal, ever: the segment stays `.open`, never `.sotseg`, and the
    // final `producer_dead` frame this path would otherwise have written
    // never lands.
    let seg_dir = root.join("seg");
    let names: Vec<String> = std::fs::read_dir(&seg_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|n| !n.ends_with(".sotseg")),
        "a terminal teardown failure must never seal a segment: {names:?}"
    );
    assert!(
        names.iter().any(|n| n.ends_with(".open")),
        "the survivor segment must remain unsealed: {names:?}"
    );
}

/// Test 13 (U1a, ADR 0041 EndRun state machine item 4 / "ack grace"): a
/// mgmt `shutdown` accepted during teardown (mirroring "a request accepted
/// in the final service poll") must have its `ShutdownAck` physically
/// written before this capsule's own transport disappears — proven by
/// holding that ack's completion and observing `run` genuinely blocks
/// rather than tearing the pipe down out from under it, then releasing it
/// and observing `run` completes promptly.
///
/// U1a Codex round-1, Blocker 3 discharge: an EARLIER version of this test
/// held the ack while running a persistent `cmd.exe` but never sent
/// `Command::Kill` -- since `AttachAction::Shutdown` (which sets
/// `shutdown_requested`) is only emitted once THIS SAME held ack is
/// reported physically written, `run` never reached teardown AT ALL, let
/// alone the grace loop, and the test could only time out. A SEPARATE
/// cause (here, `Command::Kill`) must drive the primary into teardown
/// independently of the held connection.
///
/// U1a Codex round-1, minor cluster: the "still blocked" observation is a
/// CONTINUOUS poll against `shutdown_all_call_count` (the actual
/// mechanism-relevant signal), not one sleep-then-check at an arbitrary
/// point -- a single check at, say, 500ms can pass merely because
/// ordinary Phase A/B teardown itself hadn't finished yet, proving
/// nothing about the grace specifically. Polling continuously up to a
/// GENEROUS floor (1.5s, safely under the 2s grace and safely over the
/// sub-second teardown a trivial killed `cmd.exe` takes) means ANY early
/// firing is caught the instant it happens, regardless of how long
/// ordinary teardown took to get there.
#[test]
fn shutdown_ack_grace_defers_transport_shutdown_until_the_late_ack_completes() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()]; // stays open until killed
    let cfg = config(dir.path(), "ackgrace1", argv, 80, 25);
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    // A SEPARATE cause drives the primary into teardown (an operator kill,
    // mirroring a natural producer exit just as well) -- the late mgmt
    // connection below is a RACING request, not what ends the run.
    const LATE_MGMT: ConnId = 1;
    transport.open(LATE_MGMT);
    transport.set_hold_for(LATE_MGMT, true); // never completes on its own
    transport.feed(LATE_MGMT, frame::mgmt_shutdown("late-in-teardown"));
    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("late mgmt shutdown_ok queued", LATE_MGMT, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ShutdownOk)).then_some(())
    });
    tx.send(Command::Kill).unwrap();

    // The ack's bytes are already QUEUED (proven above via `wait_for`, which
    // watches queued bytes, not completions) but its physical-write
    // completion is HELD -- `run` must not let its transport disappear
    // while that is true. Continuous poll, not one snapshot: an ORDER
    // assertion that holds regardless of how long ordinary teardown itself
    // happens to take.
    let floor = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < floor {
        assert!(!transport.shutdown_all_was_called(), "shutdown_all must not run before the grace resolves");
        assert!(!handle.is_finished(), "the ack grace must hold the transport open until the late ack completes");
        std::thread::sleep(Duration::from_millis(20));
    }

    transport.release_held();
    let summary = wait_for_join(handle, Duration::from_secs(10))
        .expect("run did not return after the late ack was released")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    assert_eq!(
        transport.shutdown_all_call_count(),
        2,
        "shutdown_all must run exactly twice: the explicit ack-grace call, then ShutdownGuard::drop"
    );

    let frames = sealed_frames(&dir.path().join("ackgrace1"), "ackgrace1");
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["reason"], "late-in-teardown");
}

/// Test 14 (U1a): the grace is a DEADLINE, not an indefinite wait — if the
/// late ack's completion never arrives, `run` still completes once
/// `SHUTDOWN_ACK_GRACE` (2s) elapses, and `shutdown_all` still runs
/// afterward.
///
/// U1a Codex round-1, Blocker 3 discharge: as in the test above, a
/// SEPARATE `Command::Kill` drives teardown -- the held connection's own
/// `shutdown` request never completes its ack, so it can never itself
/// trigger `AttachAction::Shutdown`/`shutdown_requested`.
///
/// U1a Codex round-1, minor cluster: no clock-injection seam exists for
/// `SHUTDOWN_ACK_GRACE` inside `run` (it is a real integration test
/// against a real ConPTY producer, so real time is unavoidable at this
/// level regardless) — the continuous pre-deadline poll below is the
/// ORDER assertion the review asked for, layered ON TOP of (not instead
/// of) confirming the pinned 2s bound itself: the lower-bound duration
/// check only fires if the deadline expired too early, which real-clock
/// scheduler jitter can only ever make LARGER, never smaller, so it is
/// not a source of flake in this direction.
#[test]
fn shutdown_ack_grace_expires_and_teardown_still_completes() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()]; // stays open until killed
    let cfg = config(dir.path(), "ackgrace2", argv, 80, 25);
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    const LATE_MGMT: ConnId = 1;
    transport.open(LATE_MGMT);
    transport.set_hold_for(LATE_MGMT, true); // NEVER released -- proves the deadline, not the release
    transport.feed(LATE_MGMT, frame::mgmt_shutdown("never-acked"));
    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("late mgmt shutdown_ok queued", LATE_MGMT, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ShutdownOk)).then_some(())
    });
    tx.send(Command::Kill).unwrap();

    let started = Instant::now();
    // ORDER assertion: continuously poll a floor safely under the 2s
    // grace, asserting it has NOT expired early.
    let floor = started + Duration::from_millis(1500);
    while Instant::now() < floor {
        assert!(!transport.shutdown_all_was_called(), "must not expire the grace early");
        std::thread::sleep(Duration::from_millis(20));
    }

    let summary = wait_for_join(handle, Duration::from_secs(15))
        .expect("run did not return even after the ack grace should have expired")
        .unwrap();
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_secs(2), "must honor the full grace before giving up: {elapsed:?}");
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    assert_eq!(
        transport.shutdown_all_call_count(),
        2,
        "shutdown_all must run exactly twice even when the grace expires unattended"
    );
}

/// Test 15 (U1a Codex round-1, Major 6 discharge): the grace drains only
/// what is ALREADY pending — a brand new connection arriving squarely
/// inside the grace window must be closed outright, with NO reply ever
/// sent for its request, rather than admitted and given almost none of
/// the 2s the "final service poll" guarantee actually promises.
#[test]
fn shutdown_ack_grace_admits_no_new_connections_or_bytes() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()]; // stays open until killed
    let cfg = config(dir.path(), "ackgrace3", argv, 80, 25);
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    const LATE_MGMT: ConnId = 1;
    transport.open(LATE_MGMT);
    transport.set_hold_for(LATE_MGMT, true); // held throughout -- keeps the grace open for this whole test
    transport.feed(LATE_MGMT, frame::mgmt_shutdown("late-in-teardown"));
    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("late mgmt shutdown_ok queued", LATE_MGMT, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ShutdownOk)).then_some(())
    });
    tx.send(Command::Kill).unwrap();

    // Confirm we are GENUINELY mid-grace (not merely "still doing ordinary
    // Phase A/B teardown") before probing the new-admission behavior --
    // the same continuous-poll proof the other ack-grace tests use.
    let floor = Instant::now() + Duration::from_millis(500);
    while Instant::now() < floor {
        assert!(!handle.is_finished(), "the ack grace must still be holding at this point");
        std::thread::sleep(Duration::from_millis(20));
    }

    // A brand NEW connection, opened squarely inside the confirmed-active
    // grace window: it must be closed outright, and no reply may ever be
    // sent to it.
    const NEW_CONN: ConnId = 2;
    transport.open(NEW_CONN);
    transport.feed(NEW_CONN, frame::mgmt_probe());
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !transport.closed_conns().contains(&NEW_CONN) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        transport.closed_conns().contains(&NEW_CONN),
        "a connection opened during the ack grace must be closed, never left admitted"
    );
    assert!(
        transport.sent_frames().iter().all(|(c, _)| *c != NEW_CONN),
        "no reply may ever be sent to a connection admitted during the ack grace"
    );

    transport.release_held();
    let summary = wait_for_join(handle, Duration::from_secs(10))
        .expect("run did not return after the late ack was released")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
}
