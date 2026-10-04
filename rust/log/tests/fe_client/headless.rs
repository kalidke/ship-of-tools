//! The headless client: adopted geometry, pen-free screen reads, pointerless state dir, demotion.

use super::*;

// -----------------------------------------------------------------------
// ADR 0042 amendment (2026-09-07): the HEADLESS client (`attach_headless`)
// — the daemon's own `pty.input`/`pty.screen` attach, never the frontend's.
// -----------------------------------------------------------------------

/// `type_into`'s own core mechanism, driven directly at the client level
/// (the wrapper this proves lives in `sot-backend`'s `rows::run::
/// headless`, which has no dependency on this crate's test harness): a
/// headless attach adopts the CAPSULE's own geometry (never the
/// placeholder it was constructed with), delivers one input frame, and
/// sends NO resize — proven at the wire level via the sealed voyage
/// record, the same way `first_input_takes_the_pen_and_resize_precedes_
/// the_flush` above proves resize DOES precede the flush for an ordinary
/// client.
#[test]
fn headless_attach_adopts_capsule_geometry_and_types_without_resizing() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    // A capsule sized other than the client's own 24x24 placeholder AND
    // other than the common 80x24 default, so an assertion that the
    // client ends up at (120, 40) cannot pass by coincidence.
    let mut guard = spawn_supervisor_sized(&state_dir, "--start", 120, 40, SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    let mut client = FeAttachClient::<PlatformEndpoint>::attach_headless(PlatformEndpoint::default(), h.clone(), "lu6c-headless-test".to_string())
        .expect("attach_headless");

    let deadline = Instant::now() + Duration::from_secs(30);
    while !client.is_checkpointed() {
        client.pump();
        assert!(!client.is_dead(), "client died before a checkpoint arrived: {}", client.status_line());
        assert!(Instant::now() < deadline, "timed out waiting for the first checkpoint");
        std::thread::sleep(Duration::from_millis(20));
    }
    let (rows, cols) = client.screen().size();
    assert_eq!((cols, rows), (120, 40), "a headless client must adopt the CAPSULE's own geometry");

    let marker: &[u8] = b"echo SOT_LU6C_HEADLESS_MARKER\r";
    let before = client.recorded_bytes();
    client.send_input(marker);

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        client.pump();
        if let Some(outcome) = client.last_input_outcome() {
            assert_eq!(outcome, InputOutcome::Recorded, "headless input must be recorded, not refused/unknown");
            break;
        }
        assert!(!client.is_dead(), "client died before the input was recorded: {}", client.status_line());
        assert!(Instant::now() < deadline, "timed out waiting for InputRecorded");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(client.recorded_bytes() - before, marker.len() as u64, "the WHOLE payload must be recorded, not a partial ack");

    assert!(client.shutdown(Duration::from_secs(5)), "the worker must exit within the shutdown bound");

    end_run_and_wait_verified(&conn, &voyage);

    let frames = sealed_frames(&state_dir, &voyage);
    let resize_from_headless = frames.iter().any(|f| {
        if f.class != sot_log::store::envelope::Class::ControlExchange {
            return false;
        }
        let Some(p) = f.payload.as_ref() else { return false };
        p.get("phase").and_then(|v| v.as_str()) == Some("request")
            && p.get("kind_ns").and_then(|v| v.as_str()) == Some("conpty/resize")
    });
    assert!(!resize_from_headless, "a headless client must send NO resize, ever — found one in the sealed record");

    let input_frame = frames
        .iter()
        .find(|f| {
            f.class == sot_log::store::envelope::Class::Input
                && f.source.actor.controller_id.as_deref() == Some("lu6c-headless-test")
        })
        .expect("no input frame attributed to the headless controller_id in the sealed record");
    assert_eq!(
        input_frame.payload.as_ref().and_then(|p| p.get("length")).and_then(|v| v.as_u64()),
        Some(marker.len() as u64),
        "the sealed input frame's length must match the whole payload"
    );

    let _ = command(&conn, "test-headless-a-stop", SupervisorOp::Stop);
    wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// `screen_of`'s own core mechanism: a WATCHER attach reads the checkpoint
/// and NEVER takes — proven by attaching a SECOND (ordinary) client
/// afterwards and confirming its first input still gets `take_ok`
/// immediately (the pen was free the whole time the headless read ran).
#[test]
fn headless_screen_read_never_takes_the_pen() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    // The headless watcher: attach, wait for the checkpoint, then drop —
    // exactly `screen_of`'s own shape, minus the wrapper.
    {
        let mut watcher = FeAttachClient::<PlatformEndpoint>::attach_headless(PlatformEndpoint::default(), h.clone(), "lu6c-headless-watcher".to_string())
            .expect("attach_headless");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !watcher.is_checkpointed() {
            watcher.pump();
            assert!(!watcher.is_dead(), "watcher died before a checkpoint arrived: {}", watcher.status_line());
            assert!(Instant::now() < deadline, "timed out waiting for the watcher's checkpoint");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(watcher.shutdown(Duration::from_secs(5)), "the watcher's worker must exit within the bound");
    }

    // A fresh, ORDINARY client's first input must reach DRIVING via a
    // normal, uncontested take_ok — if the watcher above had taken the
    // pen and never released it, this would stall waiting for `take_ok`.
    let (_woke, wake) = wake_flag();
    let mut client = FeAttachClient::attach(
        PlatformEndpoint::default(),
        h.clone(),
        80,
        24,
        "lu6c-post-watcher-driver".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach");
    assert!(
        poll_screen(&mut client, Duration::from_secs(30), |t| t.trim().chars().any(|c| !c.is_whitespace())),
        "no checkpoint content ever reached the second client's screen"
    );
    let marker: &[u8] = b"echo SOT_LU6C_POST_WATCHER\r\n";
    client.send_input(marker);
    let found = poll_screen(&mut client, Duration::from_secs(30), |t| t.contains("SOT_LU6C_POST_WATCHER"));
    assert!(found, "the pen was not free after the headless screen read (dead={}, status={})", client.is_dead(), client.status_line());

    drop(client);
    end_run_and_wait_verified(&conn, &voyage);
    let _ = command(&conn, "test-headless-b-stop", SupervisorOp::Stop);
    wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// Decision 28 (LU6b, attach convergence): a state dir with NO supervisor
/// ever run has no `drawer.voyage` pointer and no supervisor lane, and the
/// worker invents NO cutoff of its own for that -- the frontend's create
/// path starts this same client before the supervisor has bound its lane,
/// so "nothing there yet" is a wait ("supervisor lane not answering --
/// retrying"), bounded only by the health window. What the headless
/// callers (`type_into`/`screen_of`, each with its own deadline + dead
/// check) rely on instead is proven here: `is_checkpointed()` never flips
/// while the worker is parked in that convergence wait, and a shutdown
/// closes the worker within its bound from inside the wait.
#[test]
fn headless_attach_against_a_pointerless_state_dir_waits_and_shutdown_closes_the_worker() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state-with-no-supervisor-ever-run");
    std::fs::create_dir_all(&state_dir).unwrap();

    let mut client = FeAttachClient::<PlatformEndpoint>::attach_headless(
        PlatformEndpoint::default(),
        state_dir_hash(&state_dir),
        "lu6c-headless-deadline-test".to_string(),
    )
    .expect("attach_headless (the constructor itself never touches the network)");

    // A spell long enough for several fail-fast connect rounds and their
    // backoffs; the client must still be waiting, not dead, not checkpointed.
    let spell = Instant::now() + Duration::from_secs(2);
    while Instant::now() < spell {
        client.pump();
        assert!(!client.is_dead(), "a pointerless state dir is a wait, never a terminal client (status={})", client.status_line());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!client.is_checkpointed(), "no checkpoint can exist without a supervisor");
    assert!(
        client.status_line().contains("not answering"),
        "the wait must be the designed one (supervisor lane not answering), got: {}",
        client.status_line()
    );
    assert!(
        client.shutdown(Duration::from_secs(5)),
        "the worker must be observed closed within the shutdown bound from inside the convergence wait"
    );
}

/// Contention proof (brief §5(a), sequenced rather than raced — this
/// harness has no way to pin the interleaving at a specific frame
/// boundary, so the two orderings this test CAN force are: attach and
/// establish DRIVER-A first, THEN run the headless write, THEN send
/// driver-A's next keystroke): a headless `type_into` landing on a row an
/// ordinary client is already DRIVING must demote that driver (ADR 0041's
/// own take-epoch lattice — any `take` succeeds and demotes whoever held
/// the pen) — the headless write is delivered, driver-A's NEXT input sees
/// `input_refused_stale` and its own worker retakes AUTOMATICALLY and
/// invisibly to this test (ruling (c)), and the sealed record shows
/// EXACTLY ONE input frame per controller — no duplication, no silently
/// dropped keystroke on either side.
#[test]
fn headless_write_while_a_client_is_driving_demotes_it_without_duplicating_input() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    // 1. Driver A attaches and becomes DRIVING.
    let (_woke, wake) = wake_flag();
    let mut driver_a = FeAttachClient::attach(
        PlatformEndpoint::default(),
        h.clone(),
        80,
        24,
        "lu6c-driver-a".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach driver_a");
    assert!(
        poll_screen(&mut driver_a, Duration::from_secs(30), |t| t.trim().chars().any(|c| !c.is_whitespace())),
        "driver_a never saw a checkpoint"
    );
    driver_a.send_input(b"echo A1\r\n");
    assert!(
        poll_screen(&mut driver_a, Duration::from_secs(30), |t| t.contains("A1")),
        "driver_a's first input never landed (dead={}, status={})", driver_a.is_dead(), driver_a.status_line()
    );

    // 2. A headless write lands on the SAME row while driver_a still holds
    // the pen — must demote driver_a and succeed on its own.
    let mut headless = FeAttachClient::<PlatformEndpoint>::attach_headless(PlatformEndpoint::default(), h.clone(), "lu6c-headless-b".to_string())
        .expect("attach_headless");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !headless.is_checkpointed() {
        headless.pump();
        assert!(!headless.is_dead(), "headless died before a checkpoint: {}", headless.status_line());
        assert!(Instant::now() < deadline, "headless never reached a checkpoint");
        std::thread::sleep(Duration::from_millis(20));
    }
    headless.send_input(b"echo HEADLESSB\r");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        headless.pump();
        if let Some(outcome) = headless.last_input_outcome() {
            assert_eq!(outcome, InputOutcome::Recorded, "headless write while driver_a was driving must still be recorded (it demotes, not refuses, the prior driver)");
            break;
        }
        assert!(!headless.is_dead(), "headless died before its outcome: {}", headless.status_line());
        assert!(Instant::now() < deadline, "headless write timed out while driver_a was driving");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(headless.shutdown(Duration::from_secs(5)), "headless worker must close within the bound");

    // 3. driver_a's NEXT input: its own worker sees `input_refused_stale`,
    // re-takes automatically (ruling (c)), and this keystroke still lands
    // — entirely inside attach_client::client, invisible to this test except for
    // the eventual echo.
    driver_a.send_input(b"echo A2RETAKE\r\n");
    assert!(
        poll_screen(&mut driver_a, Duration::from_secs(30), |t| t.contains("A2RETAKE")),
        "driver_a's post-demotion retake never delivered its keystroke (dead={}, status={})", driver_a.is_dead(), driver_a.status_line()
    );

    drop(driver_a);
    end_run_and_wait_verified(&conn, &voyage);

    // 4. The sealed record: exactly ONE input frame per controller_id —
    // the headless write was not duplicated, and driver_a's two inputs
    // (before and after the demotion) were not merged, dropped, or
    // multiplied by the automatic retake.
    let frames = sealed_frames(&state_dir, &voyage);
    let count_for = |cid: &str| {
        frames
            .iter()
            .filter(|f| {
                f.class == sot_log::store::envelope::Class::Input
                    && f.source.actor.controller_id.as_deref() == Some(cid)
            })
            .count()
    };
    assert_eq!(count_for("lu6c-headless-b"), 1, "the headless write must appear EXACTLY once in the sealed record");
    // `capsule/frame.rs`'s own `run_input_wal` commits a durable `Class::Input`
    // frame on EVERY fresh idem_key BEFORE deciding stale-or-not ("input is
    // durably logged before the producer sees it", ADR 0039) — so
    // driver_a's post-demotion keystroke is not a single logged attempt:
    // its own worker's automatic retake (ruling (c)) mints a NEW idem_key,
    // which is a SECOND durably-logged `Class::Input` frame. Three, not
    // two: A1 (clean) + A2RETAKE's own refused-stale attempt + A2RETAKE's
    // successful retry. What must NEVER happen — a drop (fewer than 3) or
    // an actual double-FORWARD of the same bytes to the pty — is what the
    // `refused_stale_epoch` lifecycle fact below independently confirms:
    // exactly one of these three is refused.
    assert_eq!(
        count_for("lu6c-driver-a"), 3,
        "driver_a's record: A1 (clean) + A2RETAKE's refused-stale attempt + A2RETAKE's successful retry"
    );
    let refused_stale_count = frames
        .iter()
        .filter(|f| {
            f.class == sot_log::store::envelope::Class::Lifecycle
                && f.source.actor.controller_id.as_deref() == Some("lu6c-driver-a")
                && f.payload.as_ref().and_then(|p| p.get("fact")?.get("fact")?.as_str()) == Some("refused_stale_epoch")
        })
        .count();
    assert_eq!(refused_stale_count, 1, "exactly ONE of driver_a's attempts must be the refused-stale one the demotion causes");

    let _ = command(&conn, "test-headless-c-stop", SupervisorOp::Stop);
    wait_for_exit(&mut guard, Duration::from_secs(30));
}
