//! Lock tests: one daemon per state root (the daemon lock, `server::run`s first step).

use super::*;

#[tokio::test]
async fn second_daemon_refuses_live() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("lockl");
    env.spawn_sotd();
    poll_until(|| async { try_connect(&env.socket_path).await }, BOUND, "the first daemon to accept").await;

    let out = tokio::time::timeout(Duration::from_secs(5), sotd_on(&env, &[]).output())
        .await
        .expect("the second daemon did not exit within 5 s")
        .expect("spawn the second sotd");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "the second daemon must refuse; stderr: {stderr}");
    assert!(try_connect(&env.socket_path).await.is_some(), "the first daemon must still answer");
    env.kill_daemon_bounded().await;
}

#[tokio::test]
async fn second_daemon_waits_for_lock() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("lockw");
    std::fs::create_dir_all(state_dir(&env)).expect("create the state dir");
    let lock = sot_log::fence::try_lock_daemon(&state_dir(&env))
        .expect("open the daemon lock")
        .expect("the daemon lock is free");
    let mut daemon = sotd_on(&env, &[("RUST_LOG", "info")])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn sotd");

    // An unfenced start takes about as long as the hold below, so the hold
    // starts once the daemon reports that it is waiting on the lock.
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    let mut lines = tokio::io::BufReader::new(daemon.stdout.take().expect("piped stdout")).lines();
    tokio::spawn(async move {
        let mut seen_tx = Some(seen_tx);
        while let Ok(Some(line)) = lines.next_line().await {
            if line.contains(WAITING_FOR_LOCK) {
                if let Some(tx) = seen_tx.take() {
                    let _ = tx.send(());
                }
            }
        }
    });
    tokio::time::timeout(BOUND, seen_rx)
        .await
        .expect("the daemon did not report waiting on the lock within BOUND")
        .expect("the daemon's stdout closed before it reported waiting on the lock");

    let held_until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < held_until {
        assert!(
            try_connect(&env.socket_path).await.is_none(),
            "the daemon bound its socket while another holder had the lock"
        );
        let exited = daemon.try_wait().expect("try_wait");
        assert!(exited.is_none(), "the daemon exited while waiting for the lock: {exited:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    drop(lock);
    poll_until(
        || async { try_connect(&env.socket_path).await },
        Duration::from_secs(5),
        "the daemon to bind after the lock was released",
    )
    .await;
    tokio::time::timeout(BOUND, daemon.kill()).await.expect("kill the daemon within BOUND").expect("kill the daemon");
}

#[tokio::test]
async fn daemon_lock_timeout_exits_1() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("lockt");
    std::fs::create_dir_all(state_dir(&env)).expect("create the state dir");
    let _lock = sot_log::fence::try_lock_daemon(&state_dir(&env))
        .expect("open the daemon lock")
        .expect("the daemon lock is free");

    let out = tokio::time::timeout(BOUND, sotd_on(&env, &[("SOT_TEST_DAEMON_LOCK_WAIT_MS", "1000")]).output())
        .await
        .expect("the daemon did not give up on the lock within BOUND")
        .expect("spawn sotd");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "a lock timeout exits 1; stderr: {stderr}");
    let lock_path = sot_log::fence::daemon_lock_path(&state_dir(&env));
    assert!(stderr.contains(&lock_path.display().to_string()), "the error names the lock path: {stderr}");
    assert!(try_connect(&env.socket_path).await.is_none(), "a daemon that never got the lock never binds");
}

