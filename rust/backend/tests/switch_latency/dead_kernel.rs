//! A confirmed-dead kernel fails fast with a typed reason; a slow one gets the full timeout; startup survives a dropped connection.

use super::*;

/// Bound for a single dead-kernel round trip: half of the 10s
/// `KERNEL_REQUEST_TIMEOUT` a regression falls back to; the real-command
/// proof times the production case precisely (sub-100ms).
const DEAD_KERNEL_BOUND: Duration = Duration::from_secs(5);

/// Mirrors kernel.rs's respawn backoff floor.
const RESPAWN_BACKOFF_FLOOR: Duration = Duration::from_millis(250);

#[tokio::test]
async fn preview_get_on_a_bounded_output_file_surfaces_kernel_unavailable_fast() {
    let stub_dir = tempfile::tempdir().expect("stub dir");
    let stub = write_fake_kernel(stub_dir.path());
    // Contents don't matter — the daemon never gets far enough to
    // actually decode HDF5; the dead kernel fails before any plugin
    // logic runs. `.h5` only needs to be a BOUNDED-OUTPUT extension
    // (`is_bounded_output_plugin`) so `try_plugin_preview` surfaces the
    // reason instead of silently degrading to a bytes-level read
    // (which is the RIGHT behavior for, say, a `.jl` file, and
    // deliberately untouched by this fix).
    let env = Env::spawn_with(
        "deadkernel-preview",
        Some(&stub),
        &[("data.h5", b"not real hdf5")],
        &[("SOT_LANE_FAKE_JULIA_DIE_AFTER_N", "0")],
    );
    let mut conn = poll_until_connected(&env.socket_path).await;

    let body = async {
        do_hello(&mut conn).await;
        let started = Instant::now();
        codec::write_frame(
            &mut conn,
            &Frame::req(2, op::PREVIEW_GET, serde_json::json!({"node_id": "files:data.h5"})),
            None,
        )
        .await
        .expect("write preview.get");
        let (frame, _blob) = loop {
            let (frame, blob) = codec::read_frame(&mut conn).await.expect("read preview.get reply");
            if frame.id == 2 {
                break (frame, blob);
            }
        };
        (started.elapsed(), frame)
    };

    let (elapsed, frame) = tokio::time::timeout(BOUND, body).await.expect("exchange timed out");
    assert!(
        elapsed < DEAD_KERNEL_BOUND,
        "preview.get against a dead kernel took {elapsed:?}, expected well under {DEAD_KERNEL_BOUND:?}"
    );
    assert_kernel_unavailable(&frame, "preview.get");
}

/// Finding 9 / concurrency: N concurrent `kernel.request`s are all in
/// flight in ONE generation when it dies and each gets its typed reason;
/// then the supervisor alone starts exactly one more generation, which
/// serves.
#[tokio::test]
async fn concurrent_requests_share_one_generation_and_only_the_supervisor_respawns() {
    // N = `OFFLOOP_CONCURRENCY` (server.rs): gen 1 holds all N, so N may not exceed it.
    const N: u64 = 4;
    let n = N.to_string();
    let stub_dir = tempfile::tempdir().expect("stub dir");
    let stub = write_fake_kernel(stub_dir.path());
    let env = Env::spawn_with(
        "deadkernel-concurrency",
        Some(&stub),
        &[],
        &[
            ("SOT_LANE_FAKE_JULIA_DIE_AFTER_N", "1"),
            ("SOT_LANE_FAKE_JULIA_DIE_HOLDING_N", &n),
            ("SOT_LANE_FAKE_JULIA_HEALTHY_FROM_GEN", "2"),
        ],
    );
    let mut conn = poll_until_connected(&env.socket_path).await;

    let body = async {
        do_hello(&mut conn).await;
        // Pipeline all N requests before reading any reply — real
        // concurrency at the wire level, since kernel.request runs
        // off-loop and these all race into `Kernel::request` together.
        for id in 2..(2 + N) {
            codec::write_frame(
                &mut conn,
                &Frame::req(id, op::KERNEL_REQUEST, serde_json::json!({"kernel_op": "kernel.hello"})),
                None,
            )
            .await
            .expect("write kernel.request");
        }
        let mut seen = 0;
        while seen < N {
            let (frame, _blob) = codec::read_frame(&mut conn).await.expect("read reply");
            if frame.kind == Kind::Evt {
                continue;
            }
            assert_kernel_unavailable(&frame, "concurrent kernel.request");
            let msg = frame.payload.get("error").and_then(|e| e.as_str()).unwrap_or_default();
            assert!(msg.contains("exited mid-request"), "unexpected reason: {msg:?}");
            seen += 1;
        }
    };
    tokio::time::timeout(BOUND, body).await.expect("exchange timed out");
    // No request in flight: only the supervisor's own respawn can start gen 2.
    crate::support::poll_until(
        || {
            let n = env.spawn_marker_count();
            async move { (n >= 2).then_some(()) }
        },
        BOUND,
        "the supervisor's own respawn",
    )
    .await;
    let (_elapsed, frame) = tokio::time::timeout(
        BOUND,
        one_kernel_request(&mut conn, 2 + N, "kernel.hello"),
    )
    .await
    .expect("exchange timed out");
    assert_eq!(frame.payload["version"], "fake", "gen 2 must serve: {:?}", frame.payload);
    // Exact at any read: gen 2 never dies.
    assert_eq!(
        env.spawn_marker_count(),
        2,
        "exactly one respawn: the supervisor's own, and gen 2 serves"
    );
}

/// A child that answers `kernel.hello` and THEN dies before replying to
/// the next request must deliver the typed unavailable reason to that
/// SAME in-flight request (not a generic wire error) and record `Dead`
/// before any other caller can see a stale `Running`. The supervisor's
/// own respawn then waits out the backoff floor.
#[tokio::test]
async fn child_that_dies_mid_request_delivers_kernel_unavailable_and_marks_dead() {
    let stub_dir = tempfile::tempdir().expect("stub dir");
    let stub = write_fake_kernel(stub_dir.path());
    let env = Env::spawn_with(
        "deadkernel-midrequest",
        Some(&stub),
        &[],
        // Answers hello (request #1), dies on the very next one.
        &[("SOT_LANE_FAKE_JULIA_DIE_AFTER_N", "1")],
    );
    let mut conn = poll_until_connected(&env.socket_path).await;

    let body = async {
        do_hello(&mut conn).await;
        let first = one_kernel_request(&mut conn, 2, "kernel.hello").await;
        let second = one_kernel_request(&mut conn, 3, "kernel.hello").await;
        (first, second)
    };
    let ((_elapsed1, first), (_elapsed2, second)) =
        tokio::time::timeout(BOUND, body).await.expect("exchange timed out");

    assert_kernel_unavailable(&first, "request that killed the kernel");
    // Sent right after: the cached reason inside the floor, or gen 2's own
    // death if late; typed either way. The cached case is not
    // order-provable. preview_get's hang guard catches a Dead arm that
    // waits out the deadline, not one that waits for the next generation.
    assert_kernel_unavailable(&second, "request right after the kernel died");
    let stamps = crate::support::poll_until(
        || {
            let s = env.spawn_marker_stamps();
            async move { (s.len() >= 2).then_some(s) }
        },
        BOUND,
        "the supervisor's own respawn",
    )
    .await;
    let gap = Duration::from_nanos(stamps[1] - stamps[0]);
    // A lower bound: load only lengthens it, so it never fails correct
    // code under load. Its limits: the gap starts at gen 1's start, so
    // under load gen 1's own life can hide a missing backoff (a weaker
    // check, never a false fail); and the stamps are wall clock, so a
    // backward clock step between them can fail it.
    assert!(
        gap >= RESPAWN_BACKOFF_FLOOR,
        "respawn came {gap:?} after gen 1 started, inside the {RESPAWN_BACKOFF_FLOOR:?} backoff floor"
    );
}

/// The blocker this whole module exists to prove fixed: a slow but
/// HEALTHY boot (well past the old, now-deleted 5s hello timeout) must
/// succeed, not be killed. Real 8s subprocess sleep — no fake clock.
#[tokio::test]
async fn slow_but_healthy_boot_succeeds_within_the_request_timeout() {
    let stub_dir = tempfile::tempdir().expect("stub dir");
    let stub = write_fake_kernel(stub_dir.path());
    let env = Env::spawn_with(
        "deadkernel-slowboot",
        Some(&stub),
        &[],
        &[("SOT_LANE_FAKE_JULIA_HELLO_DELAY_S", "8")],
    );
    let mut conn = poll_until_connected(&env.socket_path).await;

    let body = async {
        do_hello(&mut conn).await;
        one_kernel_request(&mut conn, 2, "kernel.hello").await
    };
    let (elapsed, frame) = tokio::time::timeout(BOUND, body).await.expect("exchange timed out");

    assert!(
        elapsed >= Duration::from_secs(7),
        "expected the real 8s hello delay to have elapsed, got {elapsed:?}"
    );
    assert!(
        frame.payload.get("code").and_then(|c| c.as_str()) != Some("kernel_unavailable"),
        "a slow-but-healthy boot must succeed, not report kernel_unavailable: {:?}",
        frame.payload
    );
    assert_eq!(env.spawn_marker_count(), 1, "a slow healthy boot must not be killed and retried");
}

/// Finding 3: a caller giving up (its connection dropped) mid-startup
/// must not affect the supervisor at all — it keeps booting the SAME
/// child, and a LATER caller on a fresh connection gets it once it's
/// ready, with no second spawn (no orphan, no wasted respawn).
#[tokio::test]
async fn dropped_connection_during_startup_leaves_no_orphan() {
    let stub_dir = tempfile::tempdir().expect("stub dir");
    let stub = write_fake_kernel(stub_dir.path());
    let env = Env::spawn_with(
        "deadkernel-cancel",
        Some(&stub),
        &[],
        &[("SOT_LANE_FAKE_JULIA_HELLO_DELAY_S", "2")],
    );

    let body = async {
        // Connection A: trigger startup, then vanish well before the 2s
        // hello resolves — never even reads a reply.
        let mut conn_a = poll_until_connected(&env.socket_path).await;
        do_hello(&mut conn_a).await;
        codec::write_frame(
            &mut conn_a,
            &Frame::req(2, op::KERNEL_REQUEST, serde_json::json!({"kernel_op": "kernel.hello"})),
            None,
        )
        .await
        .expect("write kernel.request from connection A");
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(conn_a);
        // Give the daemon a moment to notice the close and unwind
        // connection A's own job set.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Connection B: a fresh request should still succeed once the
        // SAME startup (already ~400ms in) finishes.
        let mut conn_b = poll_until_connected(&env.socket_path).await;
        do_hello(&mut conn_b).await;
        one_kernel_request(&mut conn_b, 2, "kernel.hello").await
    };
    let (_elapsed, frame) = tokio::time::timeout(BOUND, body).await.expect("exchange timed out");

    assert!(
        frame.payload.get("code").and_then(|c| c.as_str()) != Some("kernel_unavailable"),
        "connection B must see the kernel succeed, not report unavailable: {:?}",
        frame.payload
    );
    assert_eq!(
        env.spawn_marker_count(),
        1,
        "connection A's cancellation must not have caused an orphan or a second spawn"
    );
}
