//! Attach on an ended row under the per-row guard.

use super::*;

/// ADR 0043 decision 33's guard covers its own retirement clause exactly
/// like every other lifecycle mutation: two `pty.open` requests, on two
/// separate connections, racing the SAME resting `EndedNoRespawn` row
/// concurrently must both succeed -- the second simply waits for the
/// first's own guard rather than racing a second stop/resume/reset --
/// leaving exactly one `supervise` process behind (sampled continuously
/// across the whole race, as [`capsule_stale_attach_during_backoff_spawns_no_second_authority`]
/// already does for the backoff case) and never a `contended (70)` line
/// in `sotd.log` -- the OLD symptom a second spawn racing the first's own
/// fence produces.
#[tokio::test]
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_lines, reason = "one test scenario: attach on an ended row serializes under the guard")]
async fn capsule_attach_on_ended_row_serializes_under_the_guard() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("caes");
    // Holds every activation until released, guaranteeing both are blocked
    // together for genuine contention, not scheduling luck.
    let barrier = env._tmp.path().join("caes-activation-barrier");
    env.spawn_sotd_with_env(&[("SOT_TEST_ACTIVATION_BARRIER", &barrier.to_string_lossy())]);
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "caes-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = create_res.payload["session_name"].as_str().expect("session_name").to_string();
    let state_dir_path = env.state_root.join("sot").join("workspaces").join(&workspace_id);

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;

    let (status, _process) =
        sot_log::attach_client::supervisor_client::query_status(&state_dir_path).expect("query_status before ending the run");
    sot_log::attach_client::supervisor_client::end_run(&state_dir_path, status.run.as_deref().expect("a ready capsule has a run"), "test end").expect("end_run over the lane");
    // Wait for the daemon's observer, not a raw lane probe -- pty.open decides
    // from that cached phase, so a stale read here would skip activation entirely.
    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ended_no_respawn", BOUND).await;

    let state_root = env.state_root.clone();
    let log_path = env.state_root.join("sot").join("sotd.log");
    // As `capsule_stale_attach_during_backoff_spawns_no_second_authority`'s
    // own sampler: a query failure FAILS the test rather than silently
    // counting as zero processes (a false "at most one" would prove
    // nothing), and the sampler task's own join result is checked too.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let max_seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sampler_error = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let sampler = {
        let stop = stop.clone();
        let max_seen = max_seen.clone();
        let sampler_error = sampler_error.clone();
        let state_root = state_root.clone();
        tokio::spawn(async move {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let state_root = state_root.clone();
                let outcome = tokio::task::spawn_blocking(move || try_own_supervisor_count(&state_root)).await;
                match outcome {
                    Ok(Ok(n)) => max_seen.fetch_max(n, std::sync::atomic::Ordering::Relaxed),
                    Ok(Err(e)) => {
                        *sampler_error.lock().unwrap() = Some(format!("the capsule process listing failed: {e}"));
                        return;
                    }
                    Err(join_err) => {
                        *sampler_error.lock().unwrap() = Some(format!("sampler task panicked: {join_err}"));
                        return;
                    }
                };
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
    };

    let (mut conn_a, next_id_a) = connect_and_hello(&env.socket_path).await;
    let (mut conn_b, next_id_b) = connect_and_hello(&env.socket_path).await;
    let pty_req = serde_json::json!({ "cols": 80, "rows": 24, "user_switch": true, "target": target });
    let (res_a, res_b) = tokio::join!(
        call(&mut conn_a, next_id_a, op::PTY_OPEN, pty_req.clone()),
        call(&mut conn_b, next_id_b, op::PTY_OPEN, pty_req.clone()),
    );
    // Every capsule `pty.open` (success included) carries the same
    // informational `"error"` hint text alongside `"code": "attach_direct"`
    // (`rows/ops/pty.rs`) — the CODE is the success/failure signal, never
    // `"error"`'s mere presence.
    assert_eq!(res_a.payload["code"], "attach_direct", "first concurrent pty.open on an ended row: {:?}", res_a.payload);
    assert_eq!(res_b.payload["code"], "attach_direct", "second concurrent pty.open on an ended row: {:?}", res_b.payload);

    // R4f: wait for both activations to arrive at the barrier (one marker each) --
    // a count, not a sleep, proving contention rather than merely inferring it.
    let arrivals_dir = std::path::PathBuf::from(format!("{}.arrivals", barrier.to_string_lossy()));
    poll_until(
        || {
            let dir = arrivals_dir.clone();
            async move { (count_dir_entries(&dir) >= 2).then_some(()) }
        },
        BOUND,
        "both concurrent activations to arrive at the barrier",
    )
    .await;

    // Both tasks are provably held at the barrier; exactly ONE process exists --
    // the old resting authority, not yet retired (retirement is inside the guard).
    assert!(!barrier.exists(), "test bug: the barrier must not have been released yet");
    assert_eq!(
        try_own_supervisor_count(&state_root).expect("the capsule process listing"),
        1,
        "only the old resting authority may exist while both activations are still held at the barrier"
    );

    std::fs::write(&barrier, b"go").expect("release the activation barrier");

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND.max(Duration::from_secs(90))).await;

    // R4f: wait for both activations' completion markers before sampling.
    let completions_dir = std::path::PathBuf::from(format!("{}.completions", barrier.to_string_lossy()));
    poll_until(
        || {
            let dir = completions_dir.clone();
            async move { (count_dir_entries(&dir) >= 2).then_some(()) }
        },
        BOUND,
        "both concurrent activations to complete",
    )
    .await;

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Err(join_err) = sampler.await {
        panic!("process sampler task panicked: {join_err}");
    }
    if let Some(e) = sampler_error.lock().unwrap().take() {
        panic!("process sampler failed: {e}");
    }
    assert_eq!(
        max_seen.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "expected exactly one supervise process throughout the concurrent ended-row attach"
    );
    let log_contents = std::fs::read_to_string(&log_path).unwrap_or_else(|e| panic!("could not read {log_path:?}: {e}"));
    assert!(
        !log_contents.contains("contended (70)"),
        "sotd.log has a contended (70) line — two concurrent attaches raced the retirement into the fence"
    );

    let _ = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::attach_client::supervisor_client::stop(&dir)
    })
    .await;
    env.kill_daemon_bounded().await;
}

/// Shared setup for the three round-7 tests below: a fresh capsule row,
/// Ready, then ended over the lane -- leaving its OWN resident supervisor
/// serving `ended_no_respawn`, still alive and still tracked by the
/// watchdog this daemon installed for it (ADR 0043 decision 33). Returns
/// everything a test needs to then kill that resting authority and race
/// its recovery against a fresh Selection.
#[cfg(target_os = "linux")]
async fn setup_ended_row(env: &Env, label: &str) -> (Conn, u64, String, String, PathBuf, String) {
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let create_req = serde_json::json!({
        "label": label,
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = create_res.payload["session_name"].as_str().expect("session_name").to_string();
    let state_dir_path = env.state_root.join("sot").join("workspaces").join(&workspace_id);

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;

    let (status, _process) =
        sot_log::attach_client::supervisor_client::query_status(&state_dir_path).expect("query_status before ending the run");
    let original_run = status.run.clone().expect("a ready capsule has a run");
    sot_log::attach_client::supervisor_client::end_run(&state_dir_path, status.run.as_deref().expect("a ready capsule has a run"), "test end").expect("end_run over the lane");
    // The daemon's own observer, not a raw lane probe -- `pty.open`
    // decides from that cached phase.
    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ended_no_respawn", BOUND).await;

    (conn, next_id, workspace_id, target, state_dir_path, original_run)
}

/// Shared conclusion: the row must converge on Ready in a FRESH
/// run -- never the original, never stuck `ended_no_respawn`.
#[cfg(target_os = "linux")]
async fn assert_ends_in_a_fresh_run(
    conn: &mut Conn,
    next_id: &mut u64,
    workspace_id: &str,
    state_dir_path: &Path,
    original_run: &str,
) {
    poll_for_phase(conn, next_id, workspace_id, "ready", BOUND.max(Duration::from_secs(60))).await;
    let (status2, _process2) =
        sot_log::attach_client::supervisor_client::query_status(state_dir_path).expect("query_status after the race resolved");
    let new_run = status2.run.expect("a ready capsule has a run");
    assert_ne!(
        new_run, original_run,
        "a Selection racing the watchdog must still start a NEW run, never resurrect the ended one"
    );
}

/// Codex review round 5/6 BLOCKER (and its round-8 follow-up, see the
/// fourth case below): an activation must never drop the caller's own
/// intent, and must never decide from a transient phase snapshot --
/// including a phase read AFTER the activation's own spawn, not just
/// the first probe. An ended run's resting authority is killed, so its
/// OWN watchdog crash-restarts it -- racing a Selection (`pty.open`).
/// The four cases below replace one uncontrolled race (round 6
/// SHOULD-FIX: an unbarriered race can repeatedly exercise only one
/// ordering) with deterministic control over which side reaches the
/// row's guard first, using two independent test barriers:
/// `SOT_TEST_ACTIVATION_BARRIER` (pty.open's own activation, existing)
/// and `<that path>.watchdog-restart` (the watchdog's own restart
/// attempt, round 7). Cases 1 and 3 order themselves against a THIRD
/// signal, `<that path>.waitforsettle/` -- one marker file per reprobe
/// cycle, written from inside `ensure_started`'s own loop (round 8:
/// replaces a guessed sleep with a count of the loop's own observable
/// progress).
///
/// Case 1: Selection reaches the guard first. The watchdog is held at
/// its own barrier the whole time Selection makes its first (transient)
/// check, waits, and is only then allowed through.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn a_selection_that_reaches_the_guard_first_still_retires_and_resets() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("ssf");
    let barrier = env._tmp.path().join("ssf-activation-barrier");
    let watchdog_barrier = PathBuf::from(format!("{}.watchdog-restart", barrier.to_string_lossy()));
    env.spawn_sotd_with_env(&[("SOT_TEST_ACTIVATION_BARRIER", &barrier.to_string_lossy())]);

    let (mut conn, mut next_id, workspace_id, target, state_dir_path, original_run) =
        setup_ended_row(&env, "ssf-workspace").await;

    // Neither barrier is released yet: killing the resting authority
    // lands the watchdog at ITS OWN barrier (never reaching the guard),
    // while pty.open's own activation is held at the OTHER barrier.
    kill_supervisor(&env.state_root);
    let pty_req = serde_json::json!({ "cols": 80, "rows": 24, "user_switch": true, "target": target });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);

    // Release Selection's OWN barrier first -- with the watchdog still
    // held at its own, Selection's activation is the only one that can
    // possibly reach the guard right now, proving this ordering rather
    // than merely hoping for it.
    std::fs::write(&barrier, b"go").expect("release the activation barrier");
    // Wait for a marker proving `ensure_started`'s own loop reached
    // `LockedStep::WaitForSettle` at least once -- not a guessed sleep --
    // before letting the watchdog move.
    let waitforsettle_dir = PathBuf::from(format!("{}.waitforsettle", barrier.to_string_lossy()));
    poll_until(
        || {
            let dir = waitforsettle_dir.clone();
            async move { (count_dir_entries(&dir) >= 1).then_some(()) }
        },
        BOUND,
        "Selection's activation to reach its first WaitForSettle cycle",
    )
    .await;
    std::fs::write(&watchdog_barrier, b"go").expect("release the watchdog restart barrier");

    assert_ends_in_a_fresh_run(&mut conn, &mut next_id, &workspace_id, &state_dir_path, &original_run).await;

    let _ = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::attach_client::supervisor_client::stop(&dir)
    })
    .await;
    env.kill_daemon_bounded().await;
}

/// Case 2: the watchdog reaches the guard first, runs its whole restart
/// (backoff, spawn, settle) to completion, and only THEN is Selection
/// allowed to check at all.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn a_watchdog_that_reaches_the_guard_first_still_lets_the_selection_retire_and_reset() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("wsf");
    let barrier = env._tmp.path().join("wsf-activation-barrier");
    env.spawn_sotd_with_env(&[("SOT_TEST_ACTIVATION_BARRIER", &barrier.to_string_lossy())]);
    // The watchdog's own barrier file is never created: `SOT_TEST_
    // ACTIVATION_BARRIER` is set, so `wait_for_test_watchdog_restart_
    // barrier` polls for it forever (until its own 30s bound) unless we
    // write it -- so write it immediately, letting the watchdog run
    // completely unheld while Selection stays parked.
    let watchdog_barrier = PathBuf::from(format!("{}.watchdog-restart", barrier.to_string_lossy()));
    std::fs::write(&watchdog_barrier, b"go").expect("release the watchdog restart barrier up front");

    let (mut conn, mut next_id, workspace_id, target, state_dir_path, original_run) =
        setup_ended_row(&env, "wsf-workspace").await;

    let backoff_needle = "capsule supervisor watchdog: crashed, restarting with --resume";
    let log_path = env.state_root.join("sot").join("sotd.log");
    let backoff_seen_before = count_log_occurrences(&log_path, backoff_needle);

    kill_supervisor(&env.state_root);

    // The backoff line only proves the watchdog decided to restart, not
    // that `poll_for_phase` below isn't reading a stale pre-kill
    // `ended_no_respawn`; `assert_ends_in_a_fresh_run` is the real proof.
    let backoff_deadline = Instant::now() + BOUND;
    loop {
        if count_log_occurrences(&log_path, backoff_needle) > backoff_seen_before {
            break;
        }
        assert!(Instant::now() < backoff_deadline, "timed out waiting for the watchdog's own backoff log line");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ended_no_respawn", BOUND).await;

    let pty_req = serde_json::json!({ "cols": 80, "rows": 24, "user_switch": true, "target": target });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);
    std::fs::write(&barrier, b"go").expect("release the activation barrier");

    assert_ends_in_a_fresh_run(&mut conn, &mut next_id, &workspace_id, &state_dir_path, &original_run).await;

    let _ = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::attach_client::supervisor_client::stop(&dir)
    })
    .await;
    env.kill_daemon_bounded().await;
}

/// Case 3: Selection reaches the guard first (as in case 1), but this
/// time the watchdog's own restart is held for well over one activation
/// re-probe interval before being released -- proving the wait loop
/// survives MULTIPLE transient re-checks (not just one) before the row
/// finally settles, exactly the shape of the round-6 blocker (the
/// watchdog's own settle read a still-transient phase, and only later
/// did the row actually land on `ended_no_respawn`).
#[tokio::test]
#[cfg(target_os = "linux")]
async fn a_selection_that_waits_through_several_reprobe_cycles_still_resets() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("smr");
    let barrier = env._tmp.path().join("smr-activation-barrier");
    let watchdog_barrier = PathBuf::from(format!("{}.watchdog-restart", barrier.to_string_lossy()));
    env.spawn_sotd_with_env(&[("SOT_TEST_ACTIVATION_BARRIER", &barrier.to_string_lossy())]);

    let (mut conn, mut next_id, workspace_id, target, state_dir_path, original_run) =
        setup_ended_row(&env, "smr-workspace").await;

    kill_supervisor(&env.state_root);
    let pty_req = serde_json::json!({ "cols": 80, "rows": 24, "user_switch": true, "target": target });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);

    std::fs::write(&barrier, b"go").expect("release the activation barrier");
    // A marker is written every time the wait loop re-enters
    // `LockedStep::WaitForSettle` -- waiting for SEVERAL (not just one)
    // proves the loop actually cycles, re-checking and finding the row
    // still unsettled each time, rather than merely surviving one pass,
    // before the watchdog is finally let through.
    let waitforsettle_dir = PathBuf::from(format!("{}.waitforsettle", barrier.to_string_lossy()));
    poll_until(
        || {
            let dir = waitforsettle_dir.clone();
            async move { (count_dir_entries(&dir) >= 3).then_some(()) }
        },
        BOUND,
        "Selection's activation to cycle through several WaitForSettle reprobes",
    )
    .await;
    std::fs::write(&watchdog_barrier, b"go").expect("release the watchdog restart barrier");

    assert_ends_in_a_fresh_run(&mut conn, &mut next_id, &workspace_id, &state_dir_path, &original_run).await;

    let _ = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::attach_client::supervisor_client::stop(&dir)
    })
    .await;
    env.kill_daemon_bounded().await;
}

/// Case 4 (Codex review round 8 BLOCKER, round 9's own convergence
/// fix): an ended row's supervisor was ADOPTED at boot
/// (`restart_daemon_and_prove_adoption`'s own `Alive` scenario below),
/// so no watchdog owns it; that supervisor is then stopped (exit 0,
/// which nothing restarts); a Selection probes `unreachable` with no
/// watchdog -- resting, per `is_resting_phase` -- and picks `Resume`;
/// the fresh `--resume` supervisor's own recovery
/// (`sot_log::supervisor::spawn_recovery`, wired externally through
/// `Lifecycle::Recovering` as `Starting`) is held open past
/// `SPAWN_SETTLE_DEADLINE` (2s) by `SOT_TEST_RECOVERY_DELAY_MS` (its own
/// env var, inert unless set, applies to EVERY spawn -- round 9 deleted
/// the once-per-row sentinel that used to live here) instead of needing
/// a real crash-loop recovery window (`RECOVERY_WATCHDOG`, up to about
/// 70s in production) to prove the same thing. Every `--resume` this
/// row's own retire arm spawns is delayed the same way, proving the
/// round-9 fix converges (via `own_spawn`'s identity check) without
/// ever needing a SECOND spawn to settle fast.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn a_selection_that_resumes_an_adopted_ended_row_converges_on_one_spawn_and_a_direct_reset() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("arr");
    env.spawn_sotd();
    let (conn, _next_id, workspace_id, target, state_dir_path, original_run) =
        setup_ended_row(&env, "arr-workspace").await;

    // Adopt: kill only the DAEMON, leaving the ended-but-still-resident
    // supervisor alive, then reboot with the recovery-delay hook armed.
    // Decision 33: a row this (new) daemon never itself spawned gets no
    // watchdog at all, so ONLY a Selection can ever act on it again.
    drop(conn);
    env.kill_daemon_bounded().await;
    // The barrier file exists up front, so nothing waits on it: it only turns on the activation's per-cycle markers.
    let barrier = env._tmp.path().join("arr-activation-barrier");
    std::fs::write(&barrier, b"go").expect("release the activation barrier");
    env.spawn_sotd_with_env(&[
        ("SOT_TEST_RECOVERY_DELAY_MS", "3000"),
        ("SOT_TEST_ACTIVATION_BARRIER", &barrier.to_string_lossy()),
    ]);
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ended_no_respawn", BOUND.max(Duration::from_secs(60))).await;

    // Stop the resident (adopted) supervisor itself: it exits 0, which nothing
    // restarts (a kill would be restarted by the daemon that adopted it), so
    // the row reads `unreachable` with no owner. Go straight to `pty.open`
    // (as cases 1 and 3 above do) and let ITS OWN fresh probe resume the row,
    // exactly like a real Selection would.
    stop_supervisor(&state_dir_path).await;

    // A Selection now finds `unreachable` with no watchdog -- resting --
    // and picks `Resume`. The fresh `--resume` supervisor's own recovery
    // is held open past `SPAWN_SETTLE_DEADLINE` by the env var set
    // above, so `settle_after_spawn` cannot possibly read anything but a
    // transient `starting` snapshot the first time it looks: exactly the
    // round-8 blocker's own window.
    let pty_req = serde_json::json!({ "cols": 80, "rows": 24, "user_switch": true, "target": target });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);

    // Proven the same way every other case in this file is: a FRESH
    // run, never the original, never stuck `ended_no_respawn`.
    assert_ends_in_a_fresh_run(&mut conn, &mut next_id, &workspace_id, &state_dir_path, &original_run).await;

    // Prove this test actually hit the transient-settle window it exists to exercise, not merely that the row eventually
    // recovered some other way: the activation's loop waited out `starting` at least once (the settle itself ends at the
    // first answer, so it no longer warns).
    let waitforsettle_dir = PathBuf::from(format!("{}.waitforsettle", barrier.to_string_lossy()));
    assert!(
        count_dir_entries(&waitforsettle_dir) >= 1,
        "expected at least one WaitForSettle cycle in the activation's loop"
    );

    let _ = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::attach_client::supervisor_client::stop(&dir)
    })
    .await;
    env.kill_daemon_bounded().await;
}

/// A test-owned row socket for [`capsule_attach_on_ended_row_keeps_the_row_when_stop_fails`]: it completes the hello
/// with this process's real identity, answers `row.status` as a resting `ended_no_respawn`, refuses every other op by
/// name, and records each management op it receives.
#[cfg(target_os = "linux")]
struct FailingStopLane {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ops: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    thread: std::thread::JoinHandle<()>,
}

#[cfg(target_os = "linux")]
impl FailingStopLane {
    fn bind(state_dir: &Path) -> Self {
        use std::io::{BufRead, BufReader, Write};
        use std::sync::atomic::Ordering;
        let hash = sot_platform::host::state_dir::state_dir_hash(state_dir);
        let path = sot_log::row::row_socket_path(&hash).expect("row socket path");
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind the fixture row socket");
        listener.set_nonblocking(true).expect("nonblocking listener");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ops = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let pid = std::process::id();
        let created = sot_platform::identity::challenge_unix::process_start_ticks(pid).expect("this process's start ticks");
        let thread = std::thread::spawn({
            let (stop, ops) = (stop.clone(), ops.clone());
            move || {
                while !stop.load(Ordering::Relaxed) {
                    let Ok((stream, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    };
                    stream.set_nonblocking(false).expect("blocking stream");
                    let ops = ops.clone();
                    std::thread::spawn(move || {
                        let mut reader = BufReader::new(stream.try_clone().expect("clone the stream"));
                        let mut out = stream;
                        let mut line = String::new();
                        while reader.read_line(&mut line).unwrap_or(0) > 0 {
                            let Ok(req) = serde_json::from_str::<sot_protocol::Frame>(line.trim_end()) else { return };
                            line.clear();
                            let payload = match req.op.as_str() {
                                sot_log::row::OP_HELLO => serde_json::json!({
                                    "pid": pid, "created": created, "phase": "ended_no_respawn", "run": "",
                                    "row": sot_log::row::ROW_PROTOCOL,
                                }),
                                sot_log::row::OP_STATUS => {
                                    serde_json::json!({"phase": "ended_no_respawn"})
                                }
                                other => {
                                    ops.lock().unwrap().push(other.to_string());
                                    serde_json::json!({"error": "the fixture refuses every command", "code": "refused"})
                                }
                            };
                            let mut text = serde_json::to_vec(&sot_protocol::Frame::res(req.id, &req.op, payload)).expect("a reply serializes");
                            text.push(b'\n');
                            if out.write_all(&text).is_err() {
                                return;
                            }
                        }
                    });
                }
            }
        });
        Self { stop, ops, thread }
    }

    /// Ends the lane and returns every management op it received.
    fn finish(self) -> Vec<String> {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.thread.join().expect("the fixture lane's thread");
        std::mem::take(&mut *self.ops.lock().unwrap())
    }
}

/// ADR 0043 decision 33's retirement clause: a retire whose `stop` fails leaves the row EXACTLY as it was -- still
/// registered, its end record untouched, `activation_error` holding the retire failure, and no `reset` sent. The
/// supervisor is a test-owned lane ([`FailingStopLane`]) that rests at `EndedNoRespawn` and refuses `row.stop`, so the real `ensure_started` takes its retire arm and the failure is the lane's, not an
/// induced fault in a capsule's files.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_attach_on_ended_row_keeps_the_row_when_stop_fails() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("cakw");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (workspace_id, state_dir_path) =
        create_ready_workspace_then_stop_its_supervisor(&env, &mut conn, &mut next_id, "cakw-workspace").await;
    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let target = find_row(&list_payload, &workspace_id).expect("the row is registered")["session_name"]
        .as_str()
        .expect("session_name")
        .to_string();

    let ended_path = state_dir_path.join("ended");
    let ended_before = std::fs::read(&ended_path).expect("a stopped supervisor leaves its end record");
    // The observer first reads the real supervisor's silence; only then does a different process answering on the
    // lane count as a supervisor to adopt rather than a stranger's phase to reject.
    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "unreachable", BOUND.max(Duration::from_secs(60))).await;
    let lane = FailingStopLane::bind(&state_dir_path);
    // The row's cached phase is still "unreachable", so pty.open starts an activation, whose own probe reads the fixture
    // lane's EndedNoRespawn and adopts it. pty.open answers attach_direct at once; a Stop that fails surfaces asynchronously as the row's activation_error.
    let pty_req = serde_json::json!({ "cols": 80, "rows": 24, "user_switch": true, "target": target });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);

    let activation_error_deadline = Instant::now() + BOUND;
    let error_text = loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &workspace_id) {
            if let Some(detail) = row["activation_error"].as_str() {
                break detail.to_string();
            }
        }
        assert!(
            Instant::now() < activation_error_deadline,
            "timed out waiting for the failed activation to surface as activation_error"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(
        error_text.contains("retire (stop before reset) failed"),
        "expected the retire failure in activation_error: {error_text:?}"
    );

    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    assert!(
        find_row(&list_payload, &workspace_id).is_some(),
        "the row must still be registered after a failed retirement attempt"
    );
    assert_eq!(
        std::fs::read(&ended_path).expect("the end record must still exist"),
        ended_before,
        "a failed retirement must never touch the end record"
    );
    let commands = lane.finish();
    assert!(
        commands.iter().any(|op| op == sot_log::row::OP_STOP),
        "the retire must have asked the supervisor to stop: {commands:?}"
    );
    assert!(
        !commands.iter().any(|op| op == sot_log::row::OP_RESET),
        "no reset may follow a failed stop: {commands:?}"
    );

    env.kill_daemon_bounded().await;
}

/// The daemon's cell orders on the run: a row ended and then selected is reset to a new run, and the observer's rounds
/// after it, which can read the resting supervisor's old answer, never latch it `ended_no_respawn` again.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn a_row_that_is_ended_then_selected_stays_live_through_two_observer_rounds() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first", sot_capsule_exe());

    let env = Env::new("rr2");
    env.spawn_sotd();
    let (mut conn, mut next_id, workspace_id, target, state_dir_path, original_run) = setup_ended_row(&env, "rr2-workspace").await;

    let pty_req = serde_json::json!({ "cols": 80, "rows": 24, "user_switch": true, "target": target });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);
    assert_ends_in_a_fresh_run(&mut conn, &mut next_id, &workspace_id, &state_dir_path, &original_run).await;

    // Two watch-free seconds more, each read through `workspace.list`: a row that rests at a settled phase stays there.
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        let row = find_row(&payload, &workspace_id).expect("the row stays listed");
        assert_eq!(row["phase"].as_str(), Some("ready"), "the row latched after its reset: {row:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let _ = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::attach_client::supervisor_client::stop(&dir)
    })
    .await;
    env.kill_daemon_bounded().await;
}
