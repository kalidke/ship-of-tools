//! Capsule tests over the pipe protocol: hello checkpoint, input, watcher overflow, hello refusal.

use super::*;

// ---------------------------------------------------------------------
// ADR 0041 step 5 (U2): the pipe protocol.
// ---------------------------------------------------------------------

/// ADR 0041 "attach proto v2 bound to checkpoint v2" (Codex round on
/// #194, finding 1): a connection that negotiates attach proto v1 -- an
/// OLD client's own default, predating the scrollback ring -- must get a
/// checkpoint format v1 payload: no scrollback ring, even though the
/// capsule's own live parser keeps one (`CAPSULE_SCROLLBACK_ROWS`).
/// Proves the version-gated encode path in `capsule::run`'s
/// `BeginCheckpoint` handling, independent of the ring-arrival test above
/// (which hellos at v2, the modern client's own default).
#[test]
fn hello_v1_gets_a_checkpoint_with_no_scrollback_ring() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let helper = HELPER_EXE.to_string();
    let argv = vec![
        helper,
        "--script".to_string(),
        "50".to_string(),
        "--linger".to_string(),
    ];
    let (rows, cols) = (4u16, 20u16);
    let cfg = config(dir.path(), "hellov1", argv, cols, rows);
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    // Enough real elapsed time that a v2 hello would find a nonempty
    // ring here too -- proving this test's negative result is the
    // version gate, not merely an empty ring to begin with (same pacing
    // rationale as the ring-arrival test above: `SCRIPT_BLOCK` writes
    // one line roughly every 59 ms).
    std::thread::sleep(Duration::from_millis(4000));

    const CONN: ConnId = 1;
    transport.open(CONN);
    transport.feed(CONN, frame::hello_at(wire::ATTACH_PROTO_V1));
    let mut watcher = FrameWatcher::new(&transport);
    let negotiated = watcher.wait_for("v1 hello_ok", CONN, Duration::from_secs(10), |f| {
        if let wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { proto }) = f {
            Some(*proto)
        } else {
            None
        }
    });
    assert_eq!(
        negotiated,
        wire::ATTACH_PROTO_V1,
        "the capsule must echo back exactly the negotiated version"
    );

    transport.feed(CONN, frame::attach("watcher"));
    let checkpoint_bytes =
        watcher.collect_checkpoint("v1 checkpoint", CONN, Duration::from_secs(10));

    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);

    let mut restored = vt100_ctt::Parser::new(rows, cols, 100);
    restored
        .restore_screen(&checkpoint_bytes)
        .expect("a v1 checkpoint must decode");
    restored.screen_mut().set_scrollback(usize::MAX);
    assert_eq!(
        restored.screen().scrollback(),
        0,
        "a v1-negotiated connection must receive a checkpoint with no ring"
    );
}

/// Test 8: a wire `input` from the connection that holds the pen is written to the pty and answered `input_recorded`;
/// one from a demoted connection is refused stale. No record is kept: the answer says only that the bytes were
/// written.
#[test]
fn wire_input_is_recorded_once_written_and_stale_input_is_refused() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()]; // stays open until killed
    let cfg = config(dir.path(), "inputwritten1", argv, 80, 25);
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    // conn A attaches and takes -- the first driver ever, a pipe take.
    const A: ConnId = 1;
    transport.open(A);
    transport.feed(A, frame::hello());
    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("A hello_ok", A, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.feed(A, frame::attach("alice"));
    watcher.collect_checkpoint("A checkpoint", A, Duration::from_secs(10));
    transport.feed(A, frame::take("alice"));
    let epoch = watcher.wait_for("A take_ok", A, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { take_epoch }) => Some(*take_epoch),
        _ => None,
    });

    // Fresh input while authorized: written, so recorded -- and the shell's echo shows it reached the pty.
    transport.feed(A, frame::input("alice", epoch, [0x11u8; 16], b"echo SOT_INPUT_WRITTEN_c3\r\n"));
    let outcome1 = watcher.wait_for("A input fresh outcome", A, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::InputRecorded) => Some(true),
        wire::DecodedFrame::AttachServer(wire::AttachServer::InputRefusedStale) => Some(false),
        _ => None,
    });
    assert!(outcome1, "expected the fresh input to be recorded");
    let mut seen = String::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !seen.contains("SOT_INPUT_WRITTEN_c3") {
        assert!(Instant::now() < deadline, "the written input never echoed: {seen:?}");
        seen.push_str(&watcher.wait_for("echo of the input", A, Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::Output { bytes }) => {
                Some(String::from_utf8_lossy(bytes).into_owned())
            }
            _ => None,
        }));
    }

    // conn B attaches and takes, demoting A.
    const B: ConnId = 2;
    transport.open(B);
    transport.feed(B, frame::hello());
    watcher.wait_for("B hello_ok", B, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.feed(B, frame::attach("bob"));
    watcher.collect_checkpoint("B checkpoint", B, Duration::from_secs(10));
    transport.feed(B, frame::take("bob"));
    watcher.wait_for("B take_ok", B, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { .. })).then_some(())
    });

    // A tries again with its now-stale claim: demoted, so this is refused, folded into the SAME "stale" wire reply a
    // durable epoch mismatch gets (a demoted connection is indistinguishable from one on the wire).
    transport.feed(A, frame::input("alice", epoch, [0x22u8; 16], b"echo two\r\n"));
    let outcome2 = watcher.wait_for("A input stale outcome", A, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::InputRecorded) => Some(true),
        wire::DecodedFrame::AttachServer(wire::AttachServer::InputRefusedStale) => Some(false),
        _ => None,
    });
    assert!(!outcome2, "a demoted connection's input must be refused stale");

    tx.send(Command::Kill).unwrap();
    wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
}

/// Test 9: a slow (never-draining) watcher's queued live-output bytes
/// overflow the 4 MiB per-subscriber budget and it is closed -- no wire
/// frame exists for that eviction, by design -- while the DRIVER, a
/// separate connection under the SAME flood, stays live and fully
/// functional throughout.
#[test]
fn slow_watcher_overflow_closes_while_driver_stays_live() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let helper = HELPER_EXE.to_string();
    let total: usize = 6 * 1024 * 1024; // > the 4 MiB per-watcher budget
    // --linger: the producer must OUTLIVE the post-eviction assertions.
    // Without it, the flood's completion races the eviction wait: the
    // producer can exit first, the run enters teardown, and the resize
    // below is then (correctly) not served — observed as a deterministic
    // 10 s timeout on the real windows legs while every protocol-level
    // replay of this sequence passed.
    // --wait-line: the flood starts only when the driver sends its line,
    // after the watcher's hold below, so the hold is on before any of the
    // flood is committed.
    let argv = vec![helper, "--flood".to_string(), total.to_string(), "--linger".to_string(), "--wait-line".to_string()];
    let cfg = config(dir.path(), "slowwatcher1", argv, 80, 25);
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    const DRIVER: ConnId = 1;
    const WATCHER: ConnId = 2;
    let mut watcher = FrameWatcher::new(&transport);

    transport.open(DRIVER);
    transport.feed(DRIVER, frame::hello());
    watcher.wait_for("driver hello_ok", DRIVER, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.feed(DRIVER, frame::attach("driver"));
    watcher.collect_checkpoint("driver checkpoint", DRIVER, Duration::from_secs(10));
    transport.feed(DRIVER, frame::take("driver"));
    let take_epoch = watcher.wait_for("driver take_ok", DRIVER, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { take_epoch }) => Some(*take_epoch),
        _ => None,
    });

    transport.open(WATCHER);
    transport.feed(WATCHER, frame::hello());
    watcher.wait_for("watcher hello_ok", WATCHER, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.feed(WATCHER, frame::attach("watcher"));
    watcher.collect_checkpoint("watcher checkpoint", WATCHER, Duration::from_secs(10));
    // Never drains from here on: every future send to WATCHER queues
    // forever, simulating a client that stopped reading its pipe.
    transport.set_hold_for(WATCHER, true);
    // Start the flood: the helper reads this one line before writing.
    transport.feed(DRIVER, frame::input("driver", take_epoch, [1; 16], b"go\r\n"));

    // Bounded poll for the watcher's own close -- the flood alone drives
    // this; no fixed sleep assumes when the budget actually trips.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if transport.closed_conns().contains(&WATCHER) {
            break;
        }
        assert!(Instant::now() < deadline, "watcher was never closed under a 6 MiB flood");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!transport.closed_conns().contains(&DRIVER), "the driver must stay live");

    // The driver is still fully functional: a resize still completes.
    transport.feed(DRIVER, frame::resize(100, 40));
    let resize_ok = watcher.wait_for("driver post-eviction resize outcome", DRIVER, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeOk) => Some(true),
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeRefused { .. }) => Some(false),
        _ => None,
    });
    assert!(resize_ok, "the driver must still be able to resize after the watcher's eviction");

    // The lingering producer is ended BY REQUEST — which is also the
    // honest exit_kind for this scenario.
    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(60))
        .expect("run did not return within the local deadline")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
}

/// Test 10: a refused `hello` (unsupported proto) closes only that
/// connection -- mgmt stays available (a fresh mgmt connection, per the
/// ADR: "the ADR's 'mgmt remains available' is satisfied by a fresh mgmt
/// connection"), and a LATER, protocol-compatible attach on a separate
/// connection still succeeds normally.
#[test]
fn hello_refusal_leaves_mgmt_and_later_attach_working() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()];
    let cfg = config(dir.path(), "hellorefuse1", argv, 80, 25);
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });
    let mut watcher = FrameWatcher::new(&transport);

    const MGMT: ConnId = 1;
    const BAD_HELLO: ConnId = 2;
    const GOOD: ConnId = 3;

    transport.open(MGMT);
    transport.feed(MGMT, frame::mgmt_probe());
    watcher.wait_for("mgmt probe_ok (initial)", MGMT, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ProbeOk)).then_some(())
    });

    transport.open(BAD_HELLO);
    transport.feed(
        BAD_HELLO,
        wire::encode_attach_client(&wire::AttachClient::Hello { proto: 999 }).unwrap(),
    );
    watcher.wait_for("bad_hello hello_refused", BAD_HELLO, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloRefused { .. })).then_some(())
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if transport.closed_conns().contains(&BAD_HELLO) {
            break;
        }
        assert!(Instant::now() < deadline, "the refused hello connection was never closed");
        std::thread::sleep(Duration::from_millis(10));
    }

    // Mgmt still works on its own connection -- probe AND status, the
    // latter carrying this process's own pid/creation-time/survival.
    transport.feed(MGMT, frame::mgmt_probe());
    watcher.wait_for("mgmt probe_ok (after bad hello)", MGMT, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ProbeOk)).then_some(())
    });
    transport.feed(MGMT, frame::mgmt_status());
    let (pid, survival) = watcher.wait_for("mgmt status_ok", MGMT, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::MgmtReply(wire::MgmtReply::StatusOk { pid, survival, .. }) => Some((*pid, *survival)),
        _ => None,
    });
    assert_eq!(pid, std::process::id(), "status.pid must be the capsule's OWN process id");
    assert_eq!(survival, wire::Survival::Normal);

    // A fresh, compatible attach still succeeds.
    transport.open(GOOD);
    transport.feed(GOOD, frame::hello());
    watcher.wait_for("good hello_ok", GOOD, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.feed(GOOD, frame::attach("late"));
    watcher.collect_checkpoint("good checkpoint", GOOD, Duration::from_secs(10));

    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
}
