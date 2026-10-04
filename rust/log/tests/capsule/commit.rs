//! Capsule tests for group commit: marker polling under pings and v3 watcher completion order.

use super::*;

/// A non-panicking, bounded poll for an `AttachServer::Output` frame on
/// `conn` whose bytes contain `needle` — unlike `FrameWatcher::wait_for`
/// (which panics on timeout), this returns `false` on expiry so the
/// caller can run its own cleanup (ending the run, joining threads)
/// BEFORE asserting on the result, rather than leaking a live shell/
/// thread behind an early panic mid-test.
fn poll_for_committed_marker(transport: &TestTransport, conn: ConnId, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let mut start = 0usize;
    loop {
        let frames = transport.sent_frames_from(start);
        start += frames.len();
        for (c, bytes) in &frames {
            if *c != conn {
                continue;
            }
            let mut s = wire::FrameSplitter::new();
            let (decoded, _err) = s.feed(bytes);
            for f in &decoded {
                if let wire::DecodedFrame::AttachServer(wire::AttachServer::Output { bytes }) = f {
                    if String::from_utf8_lossy(bytes).contains(needle) {
                        return true;
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Codex round on #227 (P1 discharge): the group-commit deadline
/// (`last_commit.elapsed() >= GROUP_COMMIT_WINDOW`) used to be evaluated
/// ONLY inside `output_rx.recv_timeout`'s own `Timeout` arm, so producer
/// output plus transport activity arriving faster than
/// `GROUP_COMMIT_WINDOW` apart could starve it indefinitely — the DATA was
/// always buffered correctly; only WHEN an attached watcher got to see it
/// was at risk. Proven here with a mechanism-level bound, not a lucky
/// timing sample: a `mgmt_probe()` fed on one already-open mgmt
/// connection every 10ms — comfortably faster than `GROUP_COMMIT_WINDOW`'s
/// own 50ms — for the WHOLE observation window means `output_rx.
/// recv_timeout` can never time out during it, so a Timeout-arm-only
/// regression could not commit ANYTHING in that window NO MATTER HOW LONG
/// it ran, which is what makes the bound's own exact value (500ms, 10x the
/// window -- generous headroom for this test running alongside others
/// under `cargo test`'s default parallelism, not a tight timing race)
/// irrelevant to whether this is a real proof: the ping thread below runs
/// for the bound's own FULL duration, so a regression has no window in
/// which the Timeout arm could ever fire, regardless of how loose the
/// bound is.
#[test]
fn group_commit_progresses_despite_continuous_transport_pings() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()]; // stays open until killed
    let cfg = config(dir.path(), "commitpings1", argv, 80, 25);
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    const WATCHER: ConnId = 1;
    transport.open(WATCHER);
    transport.feed(WATCHER, frame::hello());
    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("watcher hello_ok", WATCHER, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.feed(WATCHER, frame::attach("watcher"));
    watcher.collect_checkpoint("watcher checkpoint", WATCHER, Duration::from_secs(10));
    transport.feed(WATCHER, frame::take("watcher"));
    let epoch = watcher.wait_for("watcher take_ok", WATCHER, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { take_epoch }) => Some(*take_epoch),
        _ => None,
    });

    // Continuous "transport activity" for the WHOLE observation window
    // below -- see this test's own doc for why 10ms (< GROUP_COMMIT_WINDOW)
    // is the exact shape that starves a Timeout-arm-only deadline check.
    // A repeated `mgmt_probe()` on ONE already-open mgmt connection, not a
    // churn of freshly opened connections: a fresh `ConnectionOpened`
    // every 10ms, never classified, hits `attach_proto`'s own
    // `NON_WATCHER_CAP` (4) almost immediately, spamming a real, logged
    // `RecordRefusal` per ping thereafter -- and closing each one right
    // back (tried first) exercises `remove_connection`'s own driver/
    // checkpoint-slot bookkeeping on every single tick, which is exactly
    // the kind of protocol-level churn this test has no business
    // depending on: it needs "the loop wakes on transport activity",
    // nothing about admission or teardown paths. A probe on one
    // long-lived mgmt connection is real, wake-triggering "activity" with
    // none of that: lockstep, always answered, and already the protocol's
    // OWN intended shape for frequent liveness checks.
    const PINGER_MGMT: ConnId = 999;
    transport.open(PINGER_MGMT);
    let stop_pinging = Arc::new(AtomicBool::new(false));
    let ping_transport = transport.clone();
    let stop_for_pinger = Arc::clone(&stop_pinging);
    let ping_handle = std::thread::spawn(move || {
        while !stop_for_pinger.load(Ordering::Relaxed) {
            ping_transport.feed(PINGER_MGMT, frame::mgmt_probe());
            std::thread::sleep(Duration::from_millis(10));
        }
    });

    let idem_key = [0x77u8; 16];
    transport.feed(WATCHER, frame::input("watcher", epoch, idem_key, b"echo sot-commit-marker\r\n"));

    // Non-panicking, bounded poll -- cleanup below must still run even if
    // this never finds the frame, so the assertion on `found` comes AFTER
    // it, not here.
    // The mechanism under test is that the marker arrives AT ALL while pings
    // keep the loop busy (the old loop only evaluated the group-commit
    // deadline when recv_timeout expired, which continuous pings prevent —
    // the marker then never arrived). A tight wall-clock bound is not part
    // of that proof and flakes on a loaded CI runner (a Windows leg took
    // >500 ms just to echo the shell input); the bound is generous.
    let found = poll_for_committed_marker(&transport, WATCHER, "sot-commit-marker", Duration::from_secs(10));

    stop_pinging.store(true, Ordering::Relaxed);
    ping_handle.join().unwrap();
    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);

    assert!(
        found,
        "expected the shell's own echoed input to reach the attached watcher within 500ms despite a \
         transport ping every 10ms the whole time -- the group-commit deadline must be evaluated every \
         loop iteration, not only when output_rx.recv_timeout happens to time out"
    );
}

/// Extracts every `mN` marker (`m` followed by one or more ASCII
/// digits) appearing anywhere in `text`, in order — a single `Output`
/// frame's bytes can carry several markers concatenated (the shell
/// echoes as fast as it is fed, and one wire frame is not one line).
/// Used only by `v3_watcher_completion_drains_pen_and_geometry_before_
/// a_replayed_takes_own_output`'s own byte-distinguishable marker
/// scheme.
fn extract_marker_numbers(text: &str) -> Vec<u64> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'm' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if let Ok(n) = text[start..j].parse::<u64>() {
                out.push(n);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// ADR 0046 decision 3 (lane B3b1). Codex review round 1, blocker 1: at
/// a v3 watcher's checkpoint completion, `PenSnapshot` and its whole
/// drained queue must reach the transport BEFORE any request replayed
/// off that SAME completion (a `take`/`resize` that raced in behind the
/// watcher's own still-outstanding attach reply) can publish fresh
/// output via the loop's own `flush_output` (`capsule/writer_loop/output_path.rs`'s
/// `CommitTake`/`ApplyResize` arms) — reproduced as "later Output ->
/// PenSnapshot -> Geometry -> older Output" before the fix in
/// `lane/attach_proto/`'s `sent` (`CheckpointChunk` final-chunk arm), which
/// now builds this watcher's own completion FIRST and replays a held
/// frame LAST.
///
/// Round 2 review, should-fix: round 1's own version of this test could
/// not tell a QUEUED (pre-completion) `Output` frame from a NEWER
/// (post-completion, freshly flushed by the replay) one — both are just
/// `AttachServer::Output` — so moving the replay code EARLIER (between
/// `PenSnapshot` and the queue drain, the exact defect shape) still let
/// 24 newer bytes overtake the queue while every assertion here kept
/// passing. Fixed by making the two KINDS of output byte-distinguishable
/// and adding a deterministic witness that output is genuinely pending,
/// rather than merely checking that "some Output" follows `PenSnapshot`:
///
/// `B` types SEQUENCE-NUMBERED markers (`"mN\r\n"`) continuously, and —
/// critically — a background thread also WAITS for `B`'s OWN echo of
/// each one (not merely its `input`'s WAL outcome, which is a SEPARATE,
/// unrelated-in-time commit) before advancing to the next. Since `B`
/// only ever SEES an echo once `output_committed` has actually run with
/// it — the SAME call that, uniformly, also queues it for `A` — every
/// marker number `B` has confirmed-echoed by a given instant is PROVEN,
/// not assumed, to already be sitting in `A`'s own queue (`A` is still
/// mid-transfer throughout). This is the deterministic witness: `cutoff`
/// is the highest such CONFIRMED marker right before `A` is released;
/// everything at or below it is unambiguously OLD (queued), and whatever
/// marker the typer is mid-flight on AT the instant of release — sent,
/// but not yet confirmed-echoed to `B` — is unambiguously NEW: genuinely
/// still pending, from `A`'s own perspective, by construction, not by
/// luck. The assertion below requires markers of BOTH kinds to actually
/// appear, then checks EVERY old marker's position against EVERY new
/// marker's position — old before new, full order, no reversal — which
/// is exactly what the pre-fix code violates and what round 1's weaker
/// "some Output after PenSnapshot" check could not see.
#[test]
#[allow(clippy::too_many_lines, reason = "one test scenario: a v3 watcher's completion drains pen and geometry before a replayed take's output")]
fn v3_watcher_completion_drains_pen_and_geometry_before_a_replayed_takes_own_output() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()]; // stays open until killed
    let cfg = config(dir.path(), "b3b1order1", argv, 80, 25);
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    const B: ConnId = 1;
    const A: ConnId = 2;

    // B: reaches Done and takes the pen first, alone -- nobody else is
    // attached yet, so this take's own broadcast has only B to reach.
    transport.open(B);
    transport.feed(B, frame::hello_at(wire::ATTACH_PROTO_V3));
    let mut watcher_b = FrameWatcher::new(&transport);
    watcher_b.wait_for("B hello_ok", B, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.feed(B, frame::attach("driver"));
    watcher_b.collect_checkpoint("B checkpoint", B, Duration::from_secs(10));
    transport.feed(B, frame::take("driver"));
    let epoch = watcher_b.wait_for("B take_ok", B, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { take_epoch }) => Some(*take_epoch),
        _ => None,
    });

    // B types SEQUENCE-NUMBERED, byte-distinguishable markers ("mN\r\n")
    // in the background, respecting the wire's own lockstep (one
    // outstanding `input` at a time). Two speeds, switched by
    // `witness_mode`: WITNESSED (the default, below) additionally WAITS
    // for B's OWN echo of each marker before sending the next -- the
    // deterministic proof this test's `cutoff` relies on, but slow
    // enough that the capsule's own GROUP_COMMIT_WINDOW-paced periodic
    // flush (capsule/writer_loop/output_path.rs's `flush_output`, independent of any client
    // action) always catches up first, so nothing is left GENUINELY
    // pending by the time a later action's own flush_output runs --
    // proven not to reproduce the round-1 defect shape at all by an
    // earlier version of this test, mutation-checked below. Once the
    // main thread has WITNESSED enough markers (below), it flips
    // `witness_mode` to FAST: the typer keeps sending markers, paced
    // only by the wire's own lockstep (`InputRecorded`, itself far
    // faster than a full round trip through the PTY echo), with no
    // per-marker echo wait -- creating a genuine backlog the periodic
    // flush has not caught up with yet, which is what the release right
    // after actually needs to exercise the replay's own flush_output.
    // `b_last_confirmed_echo` is the highest marker number `B` has
    // itself witnessed-echoed so far (only advanced in WITNESSED mode).
    let stop_typing = Arc::new(AtomicBool::new(false));
    let witness_mode = Arc::new(AtomicBool::new(true));
    let b_last_confirmed_echo = Arc::new(AtomicU64::new(0)); // 0 = none yet; markers are 1-based
    // Highest marker number whose WAL commit (`InputRecorded` or a
    // terminal refusal/unknown) has landed WHILE in fast mode -- a much
    // TIGHTER signal than the echo witness above (no PTY round trip
    // involved, just the wire's own ordered command reply), used to
    // release A the INSTANT a real backlog exists rather than guessing
    // a sleep duration against the capsule's own 50ms flush cadence.
    let fast_wal_confirmed = Arc::new(AtomicU64::new(0));
    let type_transport = transport.clone();
    let stop_for_typer = Arc::clone(&stop_typing);
    let witness_for_typer = Arc::clone(&witness_mode);
    let confirmed_for_typer = Arc::clone(&b_last_confirmed_echo);
    let fast_wal_for_typer = Arc::clone(&fast_wal_confirmed);
    let type_handle = std::thread::spawn(move || {
        let mut watcher_type = FrameWatcher::new(&type_transport);
        let mut n: u64 = 1;
        while !stop_for_typer.load(Ordering::Relaxed) {
            let marker = format!("echo m{n}\r\n");
            let mut idem = [0u8; 16];
            idem[..8].copy_from_slice(&n.to_le_bytes());
            type_transport.feed(B, frame::input("driver", epoch, idem, marker.as_bytes()));
            let witnessing = witness_for_typer.load(Ordering::Relaxed);
            watcher_type.wait_for("typed input outcome", B, Duration::from_secs(5), |f| {
                matches!(
                    f,
                    wire::DecodedFrame::AttachServer(wire::AttachServer::InputRecorded)
                        | wire::DecodedFrame::AttachServer(wire::AttachServer::InputRefusedStale)
                        | wire::DecodedFrame::AttachServer(wire::AttachServer::InputDeliveryUnknown)
                )
                .then_some(())
            });
            if !witnessing {
                fast_wal_for_typer.fetch_max(n, Ordering::Relaxed);
            } else {
                // The witness: B's OWN echo of THIS marker, a SEPARATE,
                // asynchronous path from the input outcome above -- only
                // once this is seen is marker n PROVEN to have already
                // reached output_committed.
                let needle = format!("m{n}");
                if poll_for_committed_marker(&type_transport, B, &needle, Duration::from_secs(5)) {
                    confirmed_for_typer.fetch_max(n, Ordering::Relaxed);
                }
            }
            n += 1;
        }
    });

    // A: the v3 watcher under test. Held from before its own `attach`
    // onward -- its checkpoint chunk gets queued but never reported
    // complete, so it stays genuinely mid-transfer (`Sending`) for the
    // whole middle section below.
    transport.open(A);
    transport.feed(A, frame::hello_at(wire::ATTACH_PROTO_V3));
    let mut watcher_a = FrameWatcher::new(&transport);
    watcher_a.wait_for("A hello_ok", A, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.set_hold_for(A, true);
    transport.feed(A, frame::attach("watcher"));
    // Throwaway cursor, same technique `attach_mid_stream_checkpoint_
    // reproduces_reference_screen` uses: only confirms the chunk was
    // constructed and queued (`sent_frames` records bytes at send time,
    // held or not) without disturbing `watcher_a`'s own cursor. By the
    // time this returns, `reply_queued` is already set on A's
    // connection (that happens synchronously when the chunk's own Send
    // is constructed, well before any transport-level completion,
    // held or not) -- so the `take` fed right after this is guaranteed
    // to be HELD, not refused as a lockstep violation.
    FrameWatcher::new(&transport).wait_for("A checkpoint chunk queued (throwaway)", A, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::CheckpointChunk { .. })).then_some(())
    });

    // A races its OWN `take` in behind its still-unconfirmed attach
    // reply.
    transport.feed(A, frame::take("a-driver"));

    // While A sits held: B resizes -- Geometry broadcasts to both
    // watchers (B, Done, gets it immediately; A, mid-transfer, gets it
    // QUEUED), and B's own `ApplyResize` handling flushes whatever
    // output is pending at that moment, ALSO queued for A.
    transport.feed(B, frame::resize(100, 40));
    watcher_b.wait_for("B resize_ok", B, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeOk)).then_some(())
    });

    // A must not have received anything beyond its own held hello_ok/
    // checkpoint chunk yet -- everything above queues behind its still-
    // open transfer.
    let leaked_while_held = transport
        .sent_frames()
        .into_iter()
        .filter(|(c, _)| *c == A)
        .flat_map(|(_, bytes)| wire::FrameSplitter::new().feed(&bytes).0)
        .filter(|f| {
            matches!(
                f,
                wire::DecodedFrame::AttachServer(wire::AttachServer::Output { .. })
                    | wire::DecodedFrame::AttachServer(wire::AttachServer::PenSnapshot { .. })
                    | wire::DecodedFrame::AttachServer(wire::AttachServer::PenChanged { .. })
                    | wire::DecodedFrame::AttachServer(wire::AttachServer::Geometry { .. })
                    | wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { .. })
                    | wire::DecodedFrame::AttachServer(wire::AttachServer::TakeRefused { .. })
            )
        })
        .count();
    assert_eq!(
        leaked_while_held, 0,
        "output/PenChanged/Geometry/take outcomes committed while A is mid-transfer must all queue behind it"
    );

    // Wait until B has confirmed-echoed at least 3 markers -- all
    // unambiguously OLD, queued for A while A sat held.
    let deadline = Instant::now() + Duration::from_secs(10);
    while b_last_confirmed_echo.load(Ordering::Relaxed) < 3 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let cutoff = b_last_confirmed_echo.load(Ordering::Relaxed);
    assert!(cutoff >= 3, "expected B to have confirmed-echoed at least 3 markers before release: got {cutoff}");

    // Switch the typer to FAST mode (see its own doc) and wait for its
    // WAL commit (NOT the slower echo witness -- no PTY round trip) to
    // confirm a few markers have actually been forwarded, THEN release A
    // IMMEDIATELY, with no further sleep. This is deliberately a COUNT,
    // not a fixed delay: the capsule's own 50ms GROUP_COMMIT_WINDOW
    // periodic flush (`capsule/`) runs on its own clock the whole time
    // this test executes regardless of what this thread does, so any
    // fixed sleep comparable to or longer than it would let that
    // ordinary cycle catch up and flush this burst on its own, before
    // release ever gets a chance to -- exactly what an EARLIER version
    // of this step (a flat 150ms sleep) did, silently defeating the very
    // race this test exists to pin. Racing the release against a COUNT
    // instead keeps this test's own margin as small as the WAL round
    // trip itself allows, whatever that happens to be on the machine
    // running it.
    witness_mode.store(false, Ordering::Relaxed);
    let fast_target = cutoff + 3;
    let fast_deadline = Instant::now() + Duration::from_secs(5);
    while fast_wal_confirmed.load(Ordering::Relaxed) < fast_target && Instant::now() < fast_deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        fast_wal_confirmed.load(Ordering::Relaxed) >= fast_target,
        "expected at least 3 fast-mode markers to be WAL-confirmed before release: got {}",
        fast_wal_confirmed.load(Ordering::Relaxed)
    );

    // Release A: its checkpoint chunk's completion fires, driving Done +
    // PenSnapshot + the whole drained queue BEFORE A's OWN held take
    // replays into CommitTake, whose flush_output publishes MORE output
    // -- which must land LAST, not ahead of PenSnapshot or the queue.
    transport.set_hold_for(A, false);
    transport.release_held();

    // A's own held take must still replay into a NEWER epoch (proves the
    // race actually engaged, not merely refused or silently dropped).
    let a_take_epoch = watcher_a.wait_for("A take_ok (replayed)", A, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { take_epoch }) => Some(*take_epoch),
        _ => None,
    });
    assert!(a_take_epoch > epoch, "A's replayed take must commit a NEWER epoch than B's own take");

    // Let a little more real time pass so whatever was in flight at
    // release actually lands, then stop typing.
    std::thread::sleep(Duration::from_millis(300));
    stop_typing.store(true, Ordering::Relaxed);
    let _ = type_handle.join();
    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);

    // The full sequence A ever received, in arrival order.
    let a_sequence: Vec<wire::DecodedFrame> = transport
        .sent_frames()
        .into_iter()
        .filter(|(c, _)| *c == A)
        .flat_map(|(_, bytes)| wire::FrameSplitter::new().feed(&bytes).0)
        .collect();

    let pen_snapshot_pos = a_sequence
        .iter()
        .position(|f| matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::PenSnapshot { .. })))
        .expect("A must receive PenSnapshot as its own first v3 event");
    let geometry_pos = a_sequence
        .iter()
        .position(|f| matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::Geometry { .. })))
        .expect("the queued Geometry from B's resize must have drained to A");
    let take_ok_pos = a_sequence
        .iter()
        .position(|f| matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { .. })))
        .expect("A's own replayed take must be in this sequence too");

    assert!(geometry_pos > pen_snapshot_pos, "the queued Geometry must drain AFTER PenSnapshot: {a_sequence:?}");
    assert!(
        take_ok_pos > pen_snapshot_pos,
        "PenSnapshot must precede A's own replayed TakeOk -- the held-take path Codex reproduced: {a_sequence:?}"
    );

    // Every marker number carried by every Output frame A received,
    // paired with that frame's own position -- a single frame can carry
    // several markers concatenated, so scan for every occurrence.
    let marker_positions: Vec<(usize, u64)> = a_sequence
        .iter()
        .enumerate()
        .filter_map(|(pos, f)| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::Output { bytes }) => Some((pos, bytes)),
            _ => None,
        })
        .flat_map(|(pos, bytes)| {
            let text = String::from_utf8_lossy(bytes).into_owned();
            extract_marker_numbers(&text).into_iter().map(move |n| (pos, n))
        })
        .collect();

    // Real Windows ConPTY, unlike a raw Unix pty, can trigger conhost's
    // own asynchronous screen-buffer repaint on a resize -- re-emitting
    // text it already sent earlier at some later, unpredictable point in
    // the byte stream (a Unix pty's resize is metadata-only: TIOCSWINSZ +
    // SIGWINCH, no redraw). Under a loaded CI runner (the >500ms Windows
    // echo latency already measured elsewhere in this file) that repaint
    // can land AFTER a genuinely NEW marker's own first echo, reproducing
    // an OLD marker's text a second time -- not a real reordering of when
    // that marker actually committed, just redraw noise. Bucket each
    // marker by its FIRST position only (its real commit order); a later
    // repeat of an already-seen number is ignored rather than inflating
    // `max_old_pos`.
    let mut first_pos: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for (pos, n) in &marker_positions {
        first_pos.entry(*n).or_insert(*pos);
    }
    let old_positions: Vec<usize> = first_pos.iter().filter(|(&n, _)| n <= cutoff).map(|(_, &p)| p).collect();
    let new_positions: Vec<usize> = first_pos.iter().filter(|(&n, _)| n > cutoff).map(|(_, &p)| p).collect();

    assert!(
        !old_positions.is_empty(),
        "expected at least one witnessed-OLD marker (numbered <= {cutoff}, confirmed-echoed to B before A was \
         released) to reach A: {a_sequence:?}"
    );
    assert!(
        !new_positions.is_empty(),
        "expected at least one NOT-yet-witnessed marker (numbered > {cutoff}) to reach A -- the deterministic \
         witness this test needs, proving output really was still pending at the instant A was released: \
         {a_sequence:?}"
    );

    let max_old_pos = *old_positions.iter().max().unwrap();
    let min_new_pos = *new_positions.iter().min().unwrap();

    assert!(
        max_old_pos > pen_snapshot_pos,
        "the witnessed-OLD (queued) output must drain AFTER PenSnapshot: {a_sequence:?}"
    );
    assert!(
        max_old_pos < min_new_pos,
        "EVERY witnessed-OLD (queued) marker must arrive strictly before EVERY not-yet-witnessed (potentially \
         fresh-flushed) marker -- this is the exact regression Codex reproduced by moving the replay earlier \
         (newer output overtaking the pending queue): old positions {old_positions:?}, new positions \
         {new_positions:?}, cutoff {cutoff}, full sequence: {a_sequence:?}"
    );
}
