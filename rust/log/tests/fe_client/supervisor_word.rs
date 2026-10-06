//! Attach converges on the supervisor's word; quit before ready; the health window.

use super::*;

#[cfg(target_os = "linux")]
fn poll_until_checkpointed_collecting_statuses(
    client: &mut FeAttachClient,
    timeout: Duration,
) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    let mut statuses: Vec<String> = Vec::new();
    loop {
        client.pump();
        let s = client.status_line().to_string();
        if statuses.last() != Some(&s) {
            statuses.push(s);
        }
        if client.is_checkpointed() {
            return statuses;
        }
        assert!(
            !client.is_dead(),
            "client died before ever checkpointing (status={}, statuses seen={statuses:?})",
            client.status_line()
        );
        assert!(
            Instant::now() < deadline,
            "timed out waiting for a checkpoint (status={}, statuses seen={statuses:?})",
            client.status_line()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[cfg(target_os = "linux")]
fn attach_converges_on_the_supervisors_word() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let started = Instant::now();
    let _guard = spawn_supervisor(&state_dir, "--start", &["/bin/sh", "-c", "sleep 60"]);

    let (_woke, wake) = wake_flag();
    let mut client = FeAttachClient::attach(
        PlatformEndpoint::default(),
        h,
        80,
        24,
        "fe-client-lu6b-a".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach");

    let statuses = poll_until_checkpointed_collecting_statuses(&mut client, Duration::from_secs(30));
    let elapsed = started.elapsed();
    println!("LU6b attach_converges_on_the_supervisors_word: spawn->Checkpoint = {elapsed:?}");
    eprintln!("LU6b attach_converges_on_the_supervisors_word: statuses seen = {statuses:?}");

    assert!(
        !statuses.iter().any(|s| s.contains("voyage pipe unreachable")),
        "the OLD dead-pipe-retry status text must never appear; saw {statuses:?}"
    );

    // DEVIATION (reported per the brief's own instruction): "the supervisor
    // lane saw ONE connection from the client" has no observable surface
    // today -- `StatusOk` carries no connection count, and
    // `spawn_supervisor`'s inherited stdout/stderr carries no per-
    // connection accept log line (`run_worker`/`supervisor/`'s lane
    // handling logs nothing on accept). Dropped, as the brief's own
    // fallback instructs.
    drop(client);
}

#[test]
#[cfg(target_os = "linux")]
fn quit_is_dispatched_before_ready() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    // A slow producer (brief's own suggested shape) -- belt and braces
    // against a loaded runner, though a real measurement (LU6b's own
    // report) found Ready is reached via the LEG's own bind, independent
    // of the producer's behavior (~600ms either way): the real margin
    // this test relies on is `request_quit` being called at time ~0,
    // before the worker thread's first network round trip even starts.
    let _guard = spawn_supervisor(&state_dir, "--start", &["/bin/sh", "-c", "sleep 3; sleep 60"]);

    let (_woke, wake) = wake_flag();
    let mut client: FeAttachClient = FeAttachClient::attach(
        PlatformEndpoint::default(),
        h,
        80,
        24,
        "fe-client-lu6b-b".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach");
    client.request_quit("lu6b test: quit before ready");

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut quit_seen = false;
    loop {
        client.pump();
        if client.quit_message().is_some() {
            quit_seen = true;
        }
        assert!(
            !client.is_checkpointed(),
            "a Checkpoint must never arrive once a quit was latched before any attach completed"
        );
        if client.should_exit() || quit_seen {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the latched quit to be dispatched (status={}, quit={:?})",
            client.status_line(),
            client.quit_message()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(quit_seen, "expected a QuitMessage to appear before this loop exited");
    assert!(!client.is_checkpointed(), "a quit-before-ready session must never reach a checkpoint");

    // The brief's own acceptance criterion is exactly what the loop above
    // proved: the quit reaches the supervisor (a QuitMessage) before any
    // Checkpoint. What the supervisor DOES with an EndRun requested this
    // early is its own call, not this test's: dispatched before the leg
    // is registered as "currently running" (a real, observed outcome —
    // `Failed { detail: "no leg is currently running" }` — this exact
    // timing produced once already), the worker simply carries on toward
    // the ordinary Ready/Checkpoint path with the quit now a settled,
    // non-blocking `Failed` state (never a hang, never a second EndRun);
    // dispatched slightly later, it can just as legitimately succeed.
    // Either way this test's property already holds, so teardown here is
    // a plain kill rather than negotiating a specific quit outcome.
    drop(client);
}

/// Codex review round finding 8: the predecessor version of this test
/// only ever watched 15 of the 120s `HEALTH_WINDOW` -- proving the
/// window does not expire EARLY, but never proving it expires AT ALL.
/// This version runs the window to its real end (a genuinely slow test,
/// deliberately: `HEALTH_WINDOW` is a wall-clock constant, and expiry is
/// exactly the fact worth proving against the real clock, not a
/// shortened stand-in for it) and asserts the client reaches `Terminal`
/// with the `HealthWindowExpired` reason once it does, having stayed
/// alive for the entire slow half.
#[test]
#[cfg(target_os = "linux")]
fn unresponsive_supervisor_expires_the_health_window() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let _server = sot_log::lane::socket_unix::SocketServer::bind_supervisor(&h, 1).expect("bind a bare supervisor socket");

    let (_woke, wake) = wake_flag();
    let mid_deadline = Instant::now() + Duration::from_secs(15);
    let mut client: FeAttachClient = FeAttachClient::attach(
        PlatformEndpoint::default(),
        h,
        80,
        24,
        "fe-client-lu6b-c".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach");

    // Well inside the window: proves it does not expire early, exactly
    // as the predecessor test did.
    let mut saw_retry_status = false;
    loop {
        client.pump();
        let s = client.status_line().to_string();
        if s.to_lowercase().contains("not answering") {
            saw_retry_status = true;
        }
        assert!(!client.is_checkpointed(), "an unresponsive supervisor lane must never yield a checkpoint");
        assert!(
            !client.is_dead(),
            "must not reach Terminal well inside the 120s health window (status={s})"
        );
        if Instant::now() >= mid_deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        saw_retry_status,
        "expected the health-window retry status to appear at least once; last status = {}",
        client.status_line()
    );
    assert!(!client.is_dead(), "must not be Terminal after only a small slice of the 120s health window");

    // Past the window's own end: a generous margin beyond the constant
    // itself, so the wait is a proof of "it DOES expire," never a tight
    // race against `HEALTH_WINDOW`'s exact edge.
    let expiry_deadline = Instant::now() + sot_log::attach_client::rules::HEALTH_WINDOW + Duration::from_secs(30);
    loop {
        client.pump();
        if client.is_dead() {
            break;
        }
        assert!(
            Instant::now() < expiry_deadline,
            "expected the client to reach Terminal once the health window truly expired (status={})",
            client.status_line()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        client.status_line().to_lowercase().contains("healthwindowexpired"),
        "expected the health-window's OWN expiry reason, got status={}",
        client.status_line()
    );
}
