#![cfg(target_os = "macos")]
//! Integration tests for the macOS identity challenge
//! (`src/challenge_macos.rs`) and the `SocketClient` construction path it
//! authenticates (`connect_voyage_socket`, whose non-Linux stub this
//! milestone replaced for macOS alone). The sibling Linux file
//! (`tests/challenge_unix.rs`) is the shape this copies -- including its
//! process-isolation and `SOT_RUNTIME_DIR`-per-test devices verbatim,
//! for one source of truth rather than two silently diverging ones.
//!
//! Deliberately ABSENT, because macOS needs neither: the Linux file's
//! `ensure_established_gap()` (its pin compares the peer's start time
//! against a pre-connect anchor, and a self-connect can land both on the
//! same `_SC_CLK_TCK` tick) and its two `pin_peer_for_test` path tests
//! (there is no pin path here to force -- the audit token carries the
//! reuse generation inline). The whole file is
//! `#![cfg(target_os = "macos")]`, so it compiles away to nothing on the
//! Linux and Windows legs; the blocking `macos-latest` job is the only
//! place it runs.

use sot_log::challenge::{ChallengeOutcome, PeerAuthOutcome};
use sot_log::challenge_macos::{authenticate_server, challenge, self_pidversion};
use sot_log::exchange::VoyageMgmtExchange;
use sot_log::socket_unix::{connect_voyage_socket, ConnId, SocketServer};
use sot_log::transport::LaneEvent;
use sot_log::wire::{self, MgmtReply, MgmtRequest, Survival};
use std::time::{Duration, Instant};

/// A per-event bound used throughout (well inside `ISOLATION_TIMEOUT`, so
/// a stalled event always trips before the parent's own kill fires).
const TIMEOUT: Duration = Duration::from_secs(10);

/// The parent's hard wall-clock bound on one isolated child test.
const ISOLATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Re-invoke THIS test binary, running only `test_name`, as a child
/// process -- see `tests/challenge_unix.rs`'s identical helper, which
/// this is copied from verbatim (renamed env var only). Needed for the
/// same reason there: `isolated_runtime_dir` sets a PROCESS-global env
/// var, which two tests sharing one binary would race over.
fn run_isolated(test_name: &str) -> bool {
    if std::env::var("CHALLENGE_MACOS_TEST_CHILD").as_deref() == Ok(test_name) {
        return true;
    }
    let exe = std::env::current_exe().expect("current_exe");
    let mut child = std::process::Command::new(exe)
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env("CHALLENGE_MACOS_TEST_CHILD", test_name)
        .spawn()
        .expect("failed to spawn isolated test child");
    let deadline = Instant::now() + ISOLATION_TIMEOUT;
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => {
                assert!(
                    status.success(),
                    "isolated test {test_name} failed in its child process: {status}"
                );
                return false;
            }
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "isolated test {test_name} did not complete within {ISOLATION_TIMEOUT:?} -- killed"
                    );
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// A fresh, canonical lowercase-hyphenated UUID for one test's voyage id.
fn fresh_voyage_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Points `SOT_RUNTIME_DIR` at a fresh, mode-0700 tempdir for the
/// lifetime of the returned guard -- mirrors `tests/socket_unix.rs`'s
/// identical helper, `tempdir_in("/tmp")` and all: `$TMPDIR` on the
/// macOS runner is ~56 bytes and `sun_path` is 104 there, so the default
/// would overflow the address for real on this leg.
struct RuntimeDirGuard {
    _tmp: tempfile::TempDir,
}

fn isolated_runtime_dir() -> RuntimeDirGuard {
    let tmp = tempfile::Builder::new()
        .prefix("sot-t")
        .tempdir_in("/tmp")
        .expect("tempdir under /tmp");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::env::set_var("SOT_RUNTIME_DIR", tmp.path());
    RuntimeDirGuard { _tmp: tmp }
}

fn next_event(server: &SocketServer, timeout: Duration) -> LaneEvent {
    server
        .events()
        .recv_timeout(timeout)
        .unwrap_or_else(|e| panic!("expected a transport event within {timeout:?}, got {e}"))
}

fn expect_accepted(server: &SocketServer, timeout: Duration) -> ConnId {
    match next_event(server, timeout) {
        LaneEvent::Accepted(id) => id,
        other => panic!("expected Accepted, got {other:?}"),
    }
}

/// The `status` request has no body, so its ENCODED length alone is what
/// we wait for; the stream is byte-type, so a single write is not
/// guaranteed to surface as a single `Bytes` event.
fn await_status_request(server: &SocketServer, conn_id: ConnId, timeout: Duration) {
    let expected = wire::encode_mgmt_request(&MgmtRequest::Status).unwrap();
    let mut got = Vec::new();
    let deadline = Instant::now() + timeout;
    while got.len() < expected.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for the status request");
        match server.events().recv_timeout(remaining) {
            Ok(LaneEvent::Bytes(cid, bytes)) if cid == conn_id => got.extend(bytes),
            Ok(other) => panic!("unexpected event waiting for status: {other:?}"),
            Err(_) => panic!("timed out waiting for the status request"),
        }
    }
    assert_eq!(got, expected);
}

/// Run one full five-step challenge against a REAL socket whose server
/// is this very process -- a genuine same-user peer -- answering with
/// whatever `reply` says. Uses `connect_voyage_socket` (the FULL
/// constructor, which already runs `authenticate_server` internally, and
/// whose macOS body is itself under test here) rather than a raw
/// unchallenged connect, exactly mirroring `tests/challenge_unix.rs`'s
/// own `self_proven_challenge`: running `challenge` on top afterward is
/// legal because `authenticate_server` never consumes anything from the
/// wire.
fn challenge_with_reply(
    reply: MgmtReply,
) -> ChallengeOutcome<sot_log::challenge_macos::ChallengedProcess> {
    let voyage_id = fresh_voyage_id();
    let server = SocketServer::bind(&voyage_id, 1).expect("bind");
    let client = connect_voyage_socket(&voyage_id).expect("connect");

    std::thread::scope(|scope| {
        let challenge_handle = scope.spawn(|| {
            let mut exchange = VoyageMgmtExchange::default();
            challenge(&client, &mut exchange, Instant::now() + Duration::from_secs(30))
        });

        let conn_id = expect_accepted(&server, TIMEOUT);
        await_status_request(&server, conn_id, TIMEOUT);
        let encoded = wire::encode_mgmt_reply(&reply).unwrap();
        server.send(conn_id, encoded, None).expect("send status_ok");

        challenge_handle.join().expect("challenge thread panicked")
    })
}

fn self_status_ok(pid: u32, created: u64) -> MgmtReply {
    MgmtReply::StatusOk {
        pid,
        created,
        survival: Survival::Normal,
    }
}

/// Acceptance, half one: a real challenge over a real socket against a
/// real same-user peer is `Proven`, and the identity it carries is the
/// one the kernel reported -- this process's own pid and `pidversion`.
#[test]
fn challenge_proves_a_genuine_same_user_server() {
    if !run_isolated("challenge_proves_a_genuine_same_user_server") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let pid = std::process::id();
    let created = u64::from(self_pidversion().expect("self_pidversion"));
    match challenge_with_reply(self_status_ok(pid, created)) {
        ChallengeOutcome::Proven(p) => {
            assert_eq!(p.pid(), pid);
            assert_eq!(p.created(), created);
        }
        other => panic!("expected Proven, got {other:?}"),
    }
}

/// Acceptance, half two: a well-formed but FABRICATED reply -- right
/// account, right socket, wrong process -- is `Foreign`. The same-user
/// check upstream cannot catch this one: same account, wrong answer.
#[test]
fn challenge_rejects_a_pid_creation_mismatch_as_foreign() {
    if !run_isolated("challenge_rejects_a_pid_creation_mismatch_as_foreign") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let outcome = challenge_with_reply(self_status_ok(1, 0));
    assert!(matches!(outcome, ChallengeOutcome::Foreign), "{outcome:?}");
}

/// The step-5 ORDERING invariant, pinned as its own case: a reply whose
/// `created` is perfectly right but whose pid is wrong is `Foreign`, on
/// the pid comparison alone -- never `Undetermined`. Cheap here (both
/// comparisons are pure equality against values already in hand, with no
/// fallible OS call between them, unlike Windows' `GetProcessTimes`),
/// and that is exactly why it is worth a test: nothing else would notice
/// if the two comparisons were ever reordered or merged.
#[test]
fn challenge_rejects_a_wrong_pid_even_when_created_is_right() {
    if !run_isolated("challenge_rejects_a_wrong_pid_even_when_created_is_right") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let created = u64::from(self_pidversion().expect("self_pidversion"));
    let wrong_pid = std::process::id().wrapping_add(1);
    let outcome = challenge_with_reply(self_status_ok(wrong_pid, created));
    assert!(matches!(outcome, ChallengeOutcome::Foreign), "{outcome:?}");
}

/// The two halves of the macOS identity -- what the kernel says about a
/// PEER (`LOCAL_PEERTOKEN` on a connected socket) and what a process can
/// say about ITSELF (`task_info(TASK_AUDIT_TOKEN)`, the value a server
/// will report as its wire `created`) -- must agree when the peer IS the
/// process. Its own test because `TASK_AUDIT_TOKEN` is the one constant
/// this lane declares locally rather than taking from `libc`: a wrong
/// flavor number fails HERE, by name, instead of surfacing as a server
/// that can never be proven.
#[test]
fn self_pidversion_agrees_with_the_peer_token_for_a_self_connect() {
    if !run_isolated("self_pidversion_agrees_with_the_peer_token_for_a_self_connect") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let voyage_id = fresh_voyage_id();
    let _server = SocketServer::bind(&voyage_id, 1).expect("bind");
    let client = connect_voyage_socket(&voyage_id).expect("connect");

    let mine = self_pidversion().expect("task_info(TASK_AUDIT_TOKEN) -- is the flavor 15?");
    match authenticate_server(&client) {
        PeerAuthOutcome::Authenticated(peer) => {
            assert_eq!(
                peer.pid,
                std::process::id(),
                "the peer token named a process other than this one"
            );
            assert_eq!(
                peer.created,
                u64::from(mine),
                "LOCAL_PEERTOKEN's pidversion ({}) and task_info(TASK_AUDIT_TOKEN)'s ({mine}) \
                 disagree for THIS process -- the wire's `created` would never match",
                peer.created
            );
            assert_ne!(mine, 0, "a real process must have a non-zero pidversion");
        }
        other => panic!("expected Authenticated, got {other:?}"),
    }
}
