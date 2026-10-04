//! Capsule tests that exercise real Unix mechanism (pty spawn, signals, geometry, held EOF).

    use super::*;

    /// Test: spawn failure (a nonexistent executable) is compensated, not
    /// escaped unsealed (the Linux capsule's own known gap, deliberately
    /// not inherited here -- see `capsule.rs`'s own module doc), and
    /// `producer_dead` is still the last frame recorded. The portable
    /// twin of `windows_only::spawn_failure_is_compensated`.
    #[test]
    fn spawn_failure_is_compensated_unix() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let argv = vec!["/nonexistent/no_such_exe".to_string()];
        let cfg = config(dir.path(), "failunix1", argv, 80, 25);
        let root = cfg.voyage_root.clone();
        let (_tx, rx) = mpsc::channel();
        let mut transport = no_transport();
        let summary = capsule::run::<P>(cfg, rx, &mut transport).unwrap();
        assert_eq!(summary.exit_kind, ExitKind::SpawnFailed);
        assert_eq!(summary.exit_code, None);
        assert_eq!(summary.segments_sealed, 1);
        verify_voyage(&root, "failunix1").unwrap();

        let frames = sealed_frames(&root, "failunix1");
        let dead = assert_producer_dead_is_last(&frames);
        assert_eq!(dead["spawn_failed"], true);
        assert!(dead["exit_code"].is_null());
    }

    /// Test (ADR 0043 decision 12's own proof): a producer that exits
    /// NATURALLY, on its own, is observed by `wait()` returning `true` on
    /// the very next main-loop poll -- never by the reader thread hitting
    /// a "fatal early EOF" bail, which the capsule's own held slave
    /// descriptor exists specifically to prevent (the master cannot see
    /// EOF/EIO until `close_output_side` drops that slave, well after
    /// `wait` has already seen the exit and teardown has begun). A bare
    /// `/bin/sh -c 'exit 3'` records `ExitKind::ProducerExited` and
    /// `exit_code == Some(Code(3))`, and the record still seals
    /// verify-green -- if the held-slave contract were broken (the reader
    /// surfacing EOF BEFORE the loop ever calls `close_output_side`), this
    /// run would instead bail unsealed with a capsule-fatal error (see
    /// `capsule::run`'s own reader-error handling).
    #[test]
    fn producer_exit_is_seen_by_wait_not_by_a_fatal_eof() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let argv = shell_command("exit 3");
        let cfg = config(dir.path(), "exitcode3", argv, 80, 25);
        let root = cfg.voyage_root.clone();
        let (_tx, rx) = mpsc::channel();
        let mut transport = no_transport();
        let summary = capsule::run::<P>(cfg, rx, &mut transport).unwrap();
        assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
        assert_eq!(summary.exit_code, Some(ExitStatus::Code(3)));
        verify_voyage(&root, "exitcode3").unwrap();

        let frames = sealed_frames(&root, "exitcode3");
        let dead = assert_producer_dead_is_last(&frames);
        assert_eq!(dead["exit_code"], 3);
    }

    /// Test (ADR 0043 decision 12, review round): a producer that closes
    /// its OWN stdio and later reopens its controlling tty must not lose
    /// any output written after the reopen. Before the review round's
    /// fix, the capsule's own reader treated the first `EIO` the master
    /// reported (the instant the child's own last slave reference closed)
    /// as terminal -- even though the CAPSULE's own held slave descriptor
    /// means the master should never actually observe that at all. The
    /// script: capture the controlling tty's path, redirect stdio away
    /// from it (closing the child's OWN slave references), sleep briefly,
    /// reopen the SAME tty by path and redirect stdout/stderr back, print
    /// a marker, exit cleanly.
    #[test]
    fn output_after_a_slave_reopen_is_recorded() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let script = "tty=$(tty); exec </dev/null >/dev/null 2>&1; sleep 1; exec >\"$tty\" 2>&1; \
                      printf AFTER_REOPEN; exit 0";
        let argv = shell_command(script);
        let cfg = config(dir.path(), "slavereopen1", argv, 80, 25);
        let root = cfg.voyage_root.clone();
        let (_tx, rx) = mpsc::channel();
        let mut transport = no_transport();
        let summary = capsule::run::<P>(cfg, rx, &mut transport).unwrap();
        assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
        assert_eq!(summary.exit_code, Some(ExitStatus::Code(0)));
        verify_voyage(&root, "slavereopen1").unwrap();

        let frames = sealed_frames(&root, "slavereopen1");
        let mut all = Vec::new();
        for f in &frames {
            if f.class == Class::Producer {
                let b64 = f.payload.as_ref().unwrap()["bytes_b64"].as_str().unwrap();
                all.extend(decode_b64(b64));
            }
        }
        let text = String::from_utf8_lossy(&all);
        assert!(text.contains("AFTER_REOPEN"), "expected output recorded after the slave reopen, got: {text:?}");
    }

    /// Test (ADR 0043 decisions 13/14): a requested kill against a `sleep
    /// 600` producer tears down through `terminate_domain`
    /// (`killpg(SIGKILL)`), and the resulting `ExitStatus` is `Signal(9)`,
    /// never `Code` -- a signal death has no code at all. The durable
    /// record carries `detail.signal == 9` and NO `exit_code` key (the
    /// two are mutually exclusive additive fields, ADR 0043 decision 13).
    #[test]
    fn signal_death_records_signal() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let argv = vec!["sleep".to_string(), "600".to_string()];
        let cfg = config(dir.path(), "signaldeath1", argv, 80, 25);
        let root = cfg.voyage_root.clone();
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let mut transport = no_transport();
            capsule::run::<P>(cfg, rx, &mut transport)
        });
        std::thread::sleep(Duration::from_millis(300));
        tx.send(Command::Kill).unwrap();
        let summary = wait_for_join(handle, Duration::from_secs(30))
            .expect("run did not return within the teardown bound")
            .unwrap();
        assert_eq!(summary.exit_kind, ExitKind::Requested);
        assert_eq!(summary.exit_code, Some(ExitStatus::Signal(9)));
        verify_voyage(&root, "signaldeath1").unwrap();

        let frames = sealed_frames(&root, "signaldeath1");
        let dead = assert_producer_dead_is_last(&frames);
        assert_eq!(dead["signal"], 9);
        assert!(
            dead.get("exit_code").is_none(),
            "a signal death must never also carry an exit_code key: {dead:?}"
        );
    }

    /// Test: after a wire `Resize`, the ACTUAL pty geometry moved -- not
    /// merely the wire's own recorded disposition string. Writes `stty
    /// size\n` through the driver connection (the same protocol path a
    /// real attach client uses) and reads the shell's own echoed answer
    /// back off the live output stream, asserting it names the resized
    /// geometry exactly (an asymmetric rows/cols pair, so a transposed
    /// readback cannot pass by accident).
    #[test]
    fn resize_reaches_the_pty() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let argv = vec![SHELL_ARGV.to_string()]; // bare interactive shell
        let cfg = config(dir.path(), "resizepty1", argv, 80, 24);
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
        watcher.wait_for("driver hello_ok", CONN, Duration::from_secs(10), |f| {
            matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
        });
        transport.feed(CONN, frame::attach("driver"));
        watcher.collect_checkpoint("driver checkpoint", CONN, Duration::from_secs(10));
        transport.feed(CONN, frame::take("driver"));
        let epoch = watcher.wait_for("driver take_ok", CONN, Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { take_epoch }) => Some(*take_epoch),
            _ => None,
        });

        // An asymmetric, in-budget geometry.
        transport.feed(CONN, frame::resize(100, 40));
        let resize_ok = watcher.wait_for("resize outcome", CONN, Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeOk) => Some(true),
            wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeRefused { .. }) => Some(false),
            _ => None,
        });
        assert!(resize_ok, "an in-budget resize must succeed");

        let idem_key = [0x77u8; 16];
        transport.feed(CONN, frame::input("driver", epoch, idem_key, b"stty size\n"));
        watcher.wait_for("stty input outcome", CONN, Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRecorded) => Some(()),
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRefusedStale) => {
                panic!("stty input unexpectedly refused stale")
            }
            _ => None,
        });

        // `stty size` prints "<rows> <cols>" -- proof the ACTUAL pty
        // geometry (not merely the wire's own recorded disposition)
        // moved.
        let mut seen = String::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            seen.push_str(&watcher.wait_for("stty output", CONN, Duration::from_secs(10), |f| {
                if let wire::DecodedFrame::AttachServer(wire::AttachServer::Output { bytes }) = f {
                    Some(String::from_utf8_lossy(bytes).into_owned())
                } else {
                    None
                }
            }));
            if seen.contains("40 100") {
                break;
            }
            assert!(Instant::now() < deadline, "never saw the resized geometry echoed back: {seen:?}");
        }

        tx.send(Command::Kill).unwrap();
        let summary = wait_for_join(handle, Duration::from_secs(30))
            .expect("run did not return within the teardown bound")
            .unwrap();
        assert_eq!(summary.exit_kind, ExitKind::Requested);
    }
