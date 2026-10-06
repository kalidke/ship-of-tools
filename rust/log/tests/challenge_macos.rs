#![cfg(target_os = "macos")]
//! Integration tests for the macOS identity challenge
//! (`src/identity/challenge_macos.rs`) and the `SocketClient` construction path it
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
//!
//! The two credential-transition tests start a helper as root through `sudo -n`: where
//! that cannot run they say so and pass, except on CI (`GITHUB_ACTIONS`), where
//! that fails.

use sot_log::identity::challenge::{ChallengeOutcome, PeerAuthOutcome};
use sot_log::identity::challenge_macos::{authenticate_server, challenge, self_pidversion};
use sot_log::identity::challenge_macos::peer_euid_pid_created;
use sot_log::identity::exchange::VoyageMgmtExchange;
use sot_log::lane::attach_proto::ConnId;
use sot_log::lane::socket_unix::{connect_voyage_socket, SocketServer};
use sot_log::lane::socket_unix::SocketClient;
use sot_log::lane::transport::LaneEvent;
use sot_log::lane::wire::{self, MgmtReply, MgmtRequest, Survival};
use std::os::fd::AsRawFd;
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
/// lifetime of the returned guard -- mirrors `tests/socket_unix/`'s
/// identical helper, `tempdir_in("/tmp")` and all: `$TMPDIR` on the
/// macOS runner is ~56 bytes and `sun_path` is 104 there, so the default
/// would overflow the address for real on this leg.
struct RuntimeDirGuard {
    _tmp: tempfile::TempDir,
}

fn private_tmp() -> tempfile::TempDir {
    let tmp = tempfile::Builder::new()
        .prefix("sot-t")
        .tempdir_in("/tmp")
        .expect("tempdir under /tmp");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    tmp
}

fn isolated_runtime_dir() -> RuntimeDirGuard {
    let tmp = private_tmp();
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
) -> ChallengeOutcome<sot_log::identity::challenge_macos::ChallengedProcess> {
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

/// Step 5's second comparison, alone: a reply naming this process's own
/// pid with a wrong `created` is `Foreign`.
#[test]
fn challenge_rejects_a_wrong_created_even_when_the_pid_is_right() {
    if !run_isolated("challenge_rejects_a_wrong_created_even_when_the_pid_is_right") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let created = u64::from(self_pidversion().expect("self_pidversion"));
    let outcome = challenge_with_reply(self_status_ok(std::process::id(), created + 1));
    assert!(matches!(outcome, ChallengeOutcome::Foreign), "{outcome:?}");
}

/// Set, as `<role> <socket path> <uid>`, only in the helper a credential-transition test starts as root; it makes
/// that test run [`transition_helper`] instead.
const TRANSITION_HELPER: &str = "CHALLENGE_MACOS_TRANSITION_HELPER";

/// The helper, run as root. `connect` connects to the socket; `listen` binds it, at mode 0666 so the test's account
/// may connect, and listens. Either way the kernel records root's credentials for the connection. It then sets its
/// effective uid to `uid`, the test's own, so its live audit token names the test's account, prints
/// `sot-transition-ready <pid>` on its own line (leading newline separates libtest's partial line), and holds the
/// socket until its input ends.
fn transition_helper(spec: &str) {
    use std::io::{Read, Write};
    use std::os::unix::fs::PermissionsExt;
    let mut parts = spec.split(' ');
    let (role, path) = (parts.next().expect("role"), parts.next().expect("path"));
    let uid: libc::uid_t = parts.next().expect("uid").parse().expect("a uid");
    let (_stream, _listener) = if role == "connect" {
        (Some(std::os::unix::net::UnixStream::connect(path).expect("helper: connect")), None)
    } else {
        let listener = std::os::unix::net::UnixListener::bind(path).expect("helper: bind");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666)).expect("helper: chmod");
        (None, Some(listener))
    };
    // SAFETY: seteuid changes only this process's credentials.
    assert_eq!(unsafe { libc::seteuid(uid) }, 0, "helper: seteuid({uid}): {}", std::io::Error::last_os_error());
    let mut out = std::io::stdout();
    writeln!(out, "\nsot-transition-ready {}", std::process::id()).expect("helper: write");
    out.flush().expect("helper: flush");
    let _ = std::io::stdin().read_to_end(&mut Vec::new());
}

/// Starts this binary's `test_name` as root through `sudo -n`, as the helper for `role` on `path`, and returns it with
/// the pid it reports once ready. `None` when it cannot start here: the test then says so and passes, except on CI.
fn start_transition_helper(test_name: &str, role: &str, path: &std::path::Path) -> Option<(std::process::Child, u32)> {
    use std::io::BufRead;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let own = unsafe { libc::geteuid() };
    if own == 0 {
        return skip("this test runs as root, so root's credentials are not another account's");
    }
    let spec = format!("{TRANSITION_HELPER}={role} {} {own}", path.display());
    let spawned = std::process::Command::new("sudo")
        .args(["-n", "/usr/bin/env", &spec])
        .arg(std::env::current_exe().expect("current_exe"))
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => return skip(&format!("cannot run sudo: {e}")),
    };
    let stdout = child.stdout.take().expect("the helper's stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    // The leading newline frames the record separately from libtest's partial `test ...` line.
    // `lines()` removes the terminator; accept the exact marker only at the start of its own line.
    // Read to the end, so the helper's later output never meets a closed pipe.
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { let _ = tx.send(None); break };
            if let Some(pid) = line.strip_prefix("sot-transition-ready ") {
                let _ = tx.send(pid.parse::<u32>().ok().filter(|pid| *pid != 0));
            }
        }
    });
    match rx.recv_timeout(ISOLATION_TIMEOUT) {
        Ok(Some(pid)) => Some((child, pid)),
        failed => {
            let reason = match failed {
                Ok(_) => "credential-transition helper sent a malformed readiness record or unreadable stdout",
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) =>
                    "timed out waiting for the helper's standalone `sot-transition-ready <pid>` line",
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) =>
                    "helper stdout ended before a standalone `sot-transition-ready <pid>` line",
            };
            drop(child.stdin.take());
            let _ = child.kill();
            let _ = child.wait();
            skip(reason)
        }
    }
}

/// A test that cannot run here says so and passes, except on CI, where a silent skip is a failure to be seen.
fn skip<T>(reason: &str) -> Option<T> {
    eprintln!("skipped: {reason}");
    assert!(std::env::var_os("GITHUB_ACTIONS").is_none(), "a test skipped on CI: {reason}");
    None
}

/// MAC-ID (ADR 0049 `## User isolation`): a daemon admits a connection by the account the kernel cached when its client
/// connected, not by the client's live audit token. The helper connects as root, then takes this account's euid, so
/// its token names this account while the cached credential names root. `server/listen.rs` `admit_peer` takes its
/// euid from `peer_euid_pid_created`, which this reads on the same kind of fd.
#[test]
fn a_client_that_takes_this_accounts_euid_after_connecting_is_still_another_account() {
    const NAME: &str = "a_client_that_takes_this_accounts_euid_after_connecting_is_still_another_account";
    if let Ok(spec) = std::env::var(TRANSITION_HELPER) {
        return transition_helper(&spec);
    }
    if !run_isolated(NAME) {
        return;
    }
    let dir = private_tmp();
    let path = dir.path().join("s.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    let Some((mut helper, helper_pid)) = start_transition_helper(NAME, "connect", &path) else { return };
    let (conn, _) = listener.accept().expect("accept the helper's connection");
    let read = peer_euid_pid_created(conn.as_raw_fd());
    drop(helper.stdin.take());
    let _ = helper.wait();
    let (euid, pid, _) = read.expect("read the peer");
    assert_eq!(pid, helper_pid, "the token's pid is not the helper's");
    assert_eq!(euid, 0, "the peer's account came from its live audit token, not the credential cached at connect");
}

/// MAC-ID (ADR 0049 `## User isolation`): a client authenticates a server by the account the kernel cached when the
/// server listened, not by the server's live audit token. The helper listens as root, then takes this account's euid.
#[test]
fn a_server_that_takes_this_accounts_euid_after_listening_is_foreign() {
    const NAME: &str = "a_server_that_takes_this_accounts_euid_after_listening_is_foreign";
    if let Ok(spec) = std::env::var(TRANSITION_HELPER) {
        return transition_helper(&spec);
    }
    if !run_isolated(NAME) {
        return;
    }
    let dir = private_tmp();
    let path = dir.path().join("s.sock");
    let Some((mut helper, _)) = start_transition_helper(NAME, "listen", &path) else { return };
    let stream = std::os::unix::net::UnixStream::connect(&path).expect("connect");
    let outcome = authenticate_server(&SocketClient::from_stream_for_test(stream, 0));
    drop(helper.stdin.take());
    let _ = helper.wait();
    assert!(
        matches!(outcome, PeerAuthOutcome::Foreign),
        "{outcome:?}: the server's account came from its live audit token, not the credential cached at listen()"
    );
}
