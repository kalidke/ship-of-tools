#![cfg(target_os = "linux")]
//! Integration tests for the ADR 0043 L1-unix LU1c Linux identity
//! challenge (`src/challenge_unix.rs`) and the `SocketClient` construction
//! path it authenticates (`connect_voyage_socket`). Mirrors the
//! analogous section of `tests/pipe_win.rs` almost line for line — see
//! that file's own module doc for the process-isolation rationale this
//! copies verbatim (`CHALLENGE_UNIX_TEST_CHILD` in place of
//! `PIPE_WIN_TEST_CHILD`), and `tests/socket_unix.rs`'s own doc for the
//! `SOT_RUNTIME_DIR`-per-test isolation this also copies (including its
//! macOS `/tmp`-not-`$TMPDIR` fix — irrelevant here, since this whole
//! file is Linux-only, but kept for one copy-paste source of truth with
//! that file rather than a second, silently-diverging one).

use sot_log::challenge::{ChallengeOutcome, ChallengeableConnection, SidAuthOutcome};
use sot_log::challenge_unix::{
    self, authenticate_server, challenge, self_start_ticks, ChallengedProcess, PeerCredentials,
    SocketChallengeable,
};
use sot_log::exchange::VoyageMgmtExchange;
use sot_log::socket_unix::{
    connect_voyage_socket, voyage_socket_path, ConnId, SocketClient, SocketServer, TransportEvent,
};
use sot_log::wire::{self, MgmtReply, MgmtRequest, Survival};
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::time::{Duration, Instant};

/// A per-event bound used throughout (well inside `ISOLATION_TIMEOUT`, so
/// a stalled event always trips before the parent's own kill fires).
const TIMEOUT: Duration = Duration::from_secs(10);

/// The parent's hard wall-clock bound on one isolated child test.
const ISOLATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Re-invoke THIS test binary, running only `test_name`, as a child
/// process — see `tests/pipe_win.rs`'s identical helper, which this is
/// copied from verbatim (renamed env var only).
fn run_isolated(test_name: &str) -> bool {
    if std::env::var("CHALLENGE_UNIX_TEST_CHILD").as_deref() == Ok(test_name) {
        return true;
    }
    let exe = std::env::current_exe().expect("current_exe");
    let mut child = std::process::Command::new(exe)
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env("CHALLENGE_UNIX_TEST_CHILD", test_name)
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
/// lifetime of the returned guard — mirrors `tests/socket_unix.rs`'s
/// identical helper.
struct RuntimeDirGuard {
    _tmp: tempfile::TempDir,
}

fn isolated_runtime_dir() -> RuntimeDirGuard {
    // `tempdir_in("/tmp")`, never the default `$TMPDIR` -- see
    // `tests/socket_unix.rs`'s own doc for the macOS CI runner history
    // this guards against (this file is Linux-only, but the helper is
    // copied verbatim rather than silently drifting from its sibling).
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

fn next_event(server: &SocketServer, timeout: Duration) -> TransportEvent {
    server
        .events()
        .recv_timeout(timeout)
        .unwrap_or_else(|e| panic!("expected a transport event within {timeout:?}, got {e}"))
}

fn expect_accepted(server: &SocketServer, timeout: Duration) -> ConnId {
    match next_event(server, timeout) {
        TransportEvent::Accepted(id) => id,
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
            Ok(TransportEvent::Bytes(cid, bytes)) if cid == conn_id => got.extend(bytes),
            Ok(other) => panic!("unexpected event waiting for status: {other:?}"),
            Err(_) => panic!("timed out waiting for the status request"),
        }
    }
    assert_eq!(got, expected);
}

/// `connect_voyage_socket`'s own race-free pin (ADR 0043 decision 8)
/// requires the peer's observed start time to be STRICTLY before
/// `established` (a tie is `Undetermined` by design). In a SAME-PROCESS
/// self-connect test, both are measured within one process's own brief
/// lifetime, both quantized to `sysconf(_SC_CLK_TCK)` (10 ms on this
/// host) -- a `run_isolated` child that binds and connects almost
/// immediately after starting can genuinely land `established` on the
/// SAME tick as its own `process_start_ticks`, hitting the strict tie for
/// real (reproduced while writing this suite). A real deployment never
/// hits this: the server process already exists, with an earlier start
/// time, long before any client dials it. This sleep is the test-only
/// accommodation for the artificial "peer is myself, freshly started"
/// topology these self-tests use — never a change to the production
/// pinning logic itself, which stays exactly as strict as the ADR
/// requires.
fn ensure_established_gap() {
    std::thread::sleep(Duration::from_millis(50));
}

/// Real challenge, real socket, SAME process on both ends — a genuine
/// same-user server, proven. Also this test's own shared "give me a real
/// proven process" helper for the two tests below it. Uses
/// `connect_voyage_socket` (the FULL constructor, which already runs
/// `authenticate_server` internally) rather than a raw unchallenged
/// connect, exactly mirroring `tests/pipe_win.rs`'s own
/// `self_proven_challenge` — running the separately-typed
/// `challenge_unix::challenge` on top afterward is legal because
/// `authenticate_server` never consumes anything from the wire.
fn self_proven_challenge() -> ChallengeOutcome<sot_log::challenge_unix::ChallengedProcess> {
    let voyage_id = fresh_voyage_id();
    let server = SocketServer::bind(&voyage_id, 1).expect("bind");
    ensure_established_gap();
    let client = connect_voyage_socket(&voyage_id).expect("connect");

    std::thread::scope(|scope| {
        let challenge_handle = scope.spawn(|| {
            let mut exchange = VoyageMgmtExchange::default();
            challenge(&client, &mut exchange, Instant::now() + Duration::from_secs(30))
        });

        let conn_id = expect_accepted(&server, TIMEOUT);
        await_status_request(&server, conn_id, TIMEOUT);
        let pid = std::process::id();
        let created = self_start_ticks().expect("self_start_ticks");
        let reply = wire::encode_mgmt_reply(&MgmtReply::StatusOk {
            pid,
            created,
            survival: Survival::Normal,
        })
        .unwrap();
        server.send(conn_id, reply, None).expect("send status_ok");

        challenge_handle.join().expect("challenge thread panicked")
    })
}

#[test]
fn challenge_proves_a_genuine_same_user_server() {
    if !run_isolated("challenge_proves_a_genuine_same_user_server") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let pid = std::process::id();
    let created = self_start_ticks().expect("self_start_ticks");
    match self_proven_challenge() {
        ChallengeOutcome::Proven(p) => {
            assert_eq!(p.pid(), pid);
            assert_eq!(p.created(), created);
        }
        other => panic!("expected Proven, got {other:?}"),
    }
}

#[test]
fn challenged_process_reverify_and_wait_reflect_a_live_self_proof() {
    if !run_isolated("challenged_process_reverify_and_wait_reflect_a_live_self_proof") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let ChallengeOutcome::Proven(p) = self_proven_challenge() else {
        panic!("expected Proven")
    };
    assert!(p.reverify().unwrap());
    // Still running: this pidfd names our OWN test process.
    assert!(!p.wait(Duration::from_millis(50)).unwrap());
}

#[test]
fn challenge_rejects_a_pid_creation_mismatch_as_foreign() {
    if !run_isolated("challenge_rejects_a_pid_creation_mismatch_as_foreign") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let voyage_id = fresh_voyage_id();
    let server = SocketServer::bind(&voyage_id, 1).expect("bind");
    ensure_established_gap();
    let client = connect_voyage_socket(&voyage_id).expect("connect");

    let outcome = std::thread::scope(|scope| {
        let challenge_handle = scope.spawn(|| {
            let mut exchange = VoyageMgmtExchange::default();
            challenge(&client, &mut exchange, Instant::now() + Duration::from_secs(30))
        });

        let conn_id = expect_accepted(&server, TIMEOUT);
        await_status_request(&server, conn_id, TIMEOUT);
        // A well-formed status_ok, but a FABRICATED pid/creation that
        // does not match the real server process (this test binary
        // itself) -- the same-user check upstream cannot catch this:
        // same account, wrong reply.
        let reply = wire::encode_mgmt_reply(&MgmtReply::StatusOk {
            pid: 1,
            created: 0,
            survival: Survival::Normal,
        })
        .unwrap();
        server.send(conn_id, reply, None).expect("send status_ok");

        challenge_handle.join().expect("challenge thread panicked")
    });

    assert!(matches!(outcome, ChallengeOutcome::Foreign), "{outcome:?}");
}

/// Real deadline expiry, real pending `read`, real `conn.cancel()` — as
/// opposed to `challenge_classifies_connection_death_mid_challenge_as_undetermined`
/// below (an ordered EOF, a DIFFERENT path entirely). The server accepts
/// and receives `status`, but never replies and never closes: the
/// client's `read` is genuinely blocked until the watchdog's deadline
/// fires and `cancel()` unblocks it.
#[test]
fn challenge_cancels_a_genuinely_pending_read_when_the_deadline_expires() {
    if !run_isolated("challenge_cancels_a_genuinely_pending_read_when_the_deadline_expires") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let voyage_id = fresh_voyage_id();
    let server = SocketServer::bind(&voyage_id, 1).expect("bind");
    ensure_established_gap();
    let client = connect_voyage_socket(&voyage_id).expect("connect");

    let outcome = std::thread::scope(|scope| {
        let challenge_handle = scope.spawn(|| {
            let mut exchange = VoyageMgmtExchange::default();
            // A short but real deadline -- the server below never
            // replies, so this can only resolve via the watchdog's own
            // cancel.
            challenge(&client, &mut exchange, Instant::now() + Duration::from_millis(300))
        });

        let conn_id = expect_accepted(&server, TIMEOUT);
        await_status_request(&server, conn_id, TIMEOUT);
        // Deliberately never reply and never close -- the client's read
        // stays genuinely pending until the deadline cancels it.

        let outcome = challenge_handle.join().expect("challenge thread panicked");
        // Keep the server (and the connection) alive until the challenge
        // side has already concluded, so the server side is never what
        // ends the connection here.
        drop(server);
        outcome
    });

    assert!(matches!(outcome, ChallengeOutcome::Undetermined), "{outcome:?}");
}

#[test]
fn challenge_classifies_connection_death_mid_challenge_as_undetermined() {
    if !run_isolated("challenge_classifies_connection_death_mid_challenge_as_undetermined") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let voyage_id = fresh_voyage_id();
    let server = SocketServer::bind(&voyage_id, 1).expect("bind");
    ensure_established_gap();
    let client = connect_voyage_socket(&voyage_id).expect("connect");

    let outcome = std::thread::scope(|scope| {
        let challenge_handle = scope.spawn(|| {
            let mut exchange = VoyageMgmtExchange::default();
            challenge(&client, &mut exchange, Instant::now() + Duration::from_secs(30))
        });

        let conn_id = expect_accepted(&server, TIMEOUT);
        await_status_request(&server, conn_id, TIMEOUT);
        // The server closes without ever answering `status`.
        server.close(conn_id);

        challenge_handle.join().expect("challenge thread panicked")
    });

    assert!(matches!(outcome, ChallengeOutcome::Undetermined), "{outcome:?}");
}

/// The "child" half of `cross_process_challenge_proves_a_real_child_server`
/// below: binds a real socket server for the voyage id named by
/// `CHALLENGE_UNIX_XPROC_VOYAGE_ID`, answers exactly one `status` request
/// with THIS PROCESS's own real pid/creation time, then stays up (exiting
/// only once the parent kills it) -- mirrors `tests/pipe_win.rs`'s own
/// child role, except this one stays alive so the parent can assert on
/// its still-live pid via `SO_PEERCRED` without a reap race. A normal
/// test pass never sets that env var, so this is a silent no-op then.
#[test]
fn cross_process_challenge_server_role() {
    let Ok(voyage_id) = std::env::var("CHALLENGE_UNIX_XPROC_VOYAGE_ID") else {
        return;
    };
    let pid = std::process::id();
    let created = self_start_ticks().expect("self_start_ticks");
    // Review round fix: don't bind (hence don't become connectable) until
    // this process's OWN clock has advanced strictly past its own start
    // tick -- the parent's connect anchor is sampled essentially at the
    // moment its `connect()` succeeds, which can only happen once THIS
    // bind has already run, so this guarantees that anchor lands strictly
    // after `created` rather than risking the same tick-quantization tie
    // the self-connect tests hit (see `ensure_established_gap`'s own
    // doc). Spinning on the OBSERVED tick, not a fixed sleep, means this
    // never over- or under-shoots regardless of this kernel's own
    // `CLK_TCK`.
    while challenge_unix::boot_ticks_now().expect("boot_ticks_now") <= created {
        std::thread::sleep(Duration::from_millis(1));
    }
    let server = SocketServer::bind(&voyage_id, 1).expect("server role: bind");
    let conn_id = expect_accepted(&server, Duration::from_secs(30));
    await_status_request(&server, conn_id, Duration::from_secs(30));
    let reply = wire::encode_mgmt_reply(&MgmtReply::StatusOk {
        pid,
        created,
        survival: Survival::Normal,
    })
    .unwrap();
    server.send(conn_id, reply, None).expect("server role: send status_ok");
    // Stay up: the parent asserts against this process's pid then kills
    // it via its own `KillGuard`.
    std::thread::sleep(Duration::from_secs(30));
}

/// The real cross-process test (ADR 0043 decision 8's own acceptance
/// row): `SO_PEERCRED`, read via the CLIENT's own fd, must resolve to a
/// GENUINELY DIFFERENT process's real pid — the kernel-level fact this
/// module's whole design depends on (see `challenge_unix.rs`'s own module
/// doc for how this was verified empirically before writing the design).
#[test]
fn cross_process_challenge_proves_a_real_child_server() {
    if !run_isolated("cross_process_challenge_proves_a_real_child_server") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let voyage_id = fresh_voyage_id();
    let exe = std::env::current_exe().expect("current_exe");
    let child = std::process::Command::new(&exe)
        .arg("--exact")
        .arg("cross_process_challenge_server_role")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env("CHALLENGE_UNIX_XPROC_VOYAGE_ID", &voyage_id)
        .env_remove("CHALLENGE_UNIX_TEST_CHILD")
        .spawn()
        .expect("failed to spawn the cross-process server child");
    let child_pid = child.id();

    struct KillGuard(Option<std::process::Child>);
    impl Drop for KillGuard {
        fn drop(&mut self) {
            if let Some(mut c) = self.0.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }
    let mut guard = KillGuard(Some(child));

    // `connect_voyage_socket`'s own bounded connect retry (ENOENT/
    // ECONNREFUSED) absorbs the child's own startup race (it hasn't
    // bound yet) -- no extra synchronization needed.
    let client = connect_voyage_socket(&voyage_id).expect("connect to the cross-process server");
    let mut exchange = VoyageMgmtExchange::default();
    let outcome = challenge(&client, &mut exchange, Instant::now() + Duration::from_secs(30));

    match outcome {
        ChallengeOutcome::Proven(p) => {
            assert_eq!(p.pid(), child_pid, "the challenged pid must be the REAL CHILD's, not our own");
            assert_ne!(p.pid(), std::process::id(), "a same-process pid here would prove nothing cross-process");
        }
        other => panic!("expected Proven against a real cross-process server, got {other:?}"),
    }

    // The child already answered and is expected to stay up until
    // killed; reap it normally here, then defuse the guard's own kill (a
    // no-op by then, kept only for the panic/early-return paths above).
    if let Some(c) = guard.0.as_mut() {
        let _ = c.kill();
        let _ = c.wait();
    }
    guard.0 = None;
}

/// The pass case: against a genuine same-account server,
/// `connect_voyage_socket`'s same-user authentication is a transparent
/// pass-through -- the connection it hands back is fully usable for the
/// caller's own intended protocol (an ordinary mgmt round trip here), not
/// merely "connected".
#[test]
fn connect_voyage_socket_authentication_pass_against_a_genuine_server() {
    if !run_isolated("connect_voyage_socket_authentication_pass_against_a_genuine_server") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let voyage_id = fresh_voyage_id();
    let server = SocketServer::bind(&voyage_id, 1).expect("bind");
    ensure_established_gap();
    let client = connect_voyage_socket(&voyage_id)
        .expect("authentication-enforced connect must pass against a genuine same-account server");

    let conn_id = expect_accepted(&server, TIMEOUT);
    let probe = wire::encode_mgmt_request(&MgmtRequest::Probe).unwrap();
    client.write_all(&probe).expect("the connection must still be fully usable after authentication");
    let mut got = Vec::new();
    let deadline = Instant::now() + TIMEOUT;
    while got.len() < probe.len() {
        match server.events().recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(TransportEvent::Bytes(cid, bytes)) if cid == conn_id => got.extend(bytes),
            other => panic!("unexpected event waiting for probe: {other:?}"),
        }
    }
    assert_eq!(got, probe);

    let reply = wire::encode_mgmt_reply(&MgmtReply::ProbeOk).unwrap();
    server.send(conn_id, reply.clone(), None).expect("send probe_ok");
    let mut buf = [0u8; 512];
    let n = client.read(&mut buf).expect("read probe_ok");
    assert_eq!(&buf[..n], reply.as_slice());
}

/// A `SocketChallengeable` whose `raw_fd()` is deliberately invalid (-1)
/// — proves `authenticate_server`'s own typed-failure surface with no
/// real socket, process, or account boundary needed: step 1
/// (`SO_PEERCRED`) itself fails against this fd, deterministically, on
/// every run. `write_all`/`read`/`cancel` are never reached
/// (`authenticate_server` never touches `IdentityExchange`, unlike the
/// full `challenge()`) -- `unreachable!()` makes that a loud, checked
/// assumption rather than a silent one.
struct InvalidFdConn;
impl ChallengeableConnection for InvalidFdConn {
    fn write_all(&self, _bytes: &[u8]) -> std::io::Result<()> {
        unreachable!("authenticate_server never sends a request")
    }
    fn read(&self, _buf: &mut [u8]) -> std::io::Result<usize> {
        unreachable!("authenticate_server never reads a reply")
    }
    fn cancel(&self) {
        unreachable!("authenticate_server never arms a reply watchdog")
    }
}
impl SocketChallengeable for InvalidFdConn {
    fn raw_fd(&self) -> RawFd {
        -1
    }
    fn connect_anchor_boot_ticks(&self) -> u64 {
        0
    }
}

#[test]
fn authenticate_server_is_undetermined_when_step_one_itself_fails() {
    let outcome = authenticate_server(&InvalidFdConn);
    assert!(matches!(outcome, SidAuthOutcome::Undetermined), "{outcome:?}");
}

/// The race-free pin's own strict inequality (ADR 0043 decision 8): a
/// peer whose observed start time equals the connect anchor (a self-
/// connected client, the anchor set to OUR OWN process's real start
/// time) is a TIE, never trusted -- `Undetermined`. One tick later, the
/// SAME peer is `Proven`. Uses `SocketClient::from_stream_for_test` to
/// control `connect_anchor_boot_ticks` directly, driving the real
/// `authenticate_server` path (steps 1-3) rather than reaching into
/// `pin_peer` itself.
#[test]
fn pin_rejects_a_peer_that_started_at_or_after_the_connection() {
    if !run_isolated("pin_rejects_a_peer_that_started_at_or_after_the_connection") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let server = SocketServer::bind(&id, 2).expect("bind");
    let path = voyage_socket_path(&id).unwrap();

    let own_start = self_start_ticks().expect("self_start_ticks");

    {
        let stream = std::os::unix::net::UnixStream::connect(&path).unwrap();
        let conn_id = expect_accepted(&server, TIMEOUT);
        let client = SocketClient::from_stream_for_test(stream, own_start);
        assert!(
            matches!(authenticate_server(&client), SidAuthOutcome::Undetermined),
            "a tie between the peer's start time and `established` must be Undetermined, never Proven"
        );
        drop(client);
        // Drain this connection's own `Closed` before opening a second
        // one below -- otherwise its (asynchronous) arrival can race the
        // second connection's own `Accepted`, since both share one
        // server-wide events channel.
        match next_event(&server, TIMEOUT) {
            TransportEvent::Closed(id, _) => assert_eq!(id, conn_id),
            other => panic!("expected Closed for the first (dropped) connection, got {other:?}"),
        }
    }
    {
        let stream = std::os::unix::net::UnixStream::connect(&path).unwrap();
        expect_accepted(&server, TIMEOUT);
        let client = SocketClient::from_stream_for_test(stream, own_start + 1);
        assert!(
            matches!(authenticate_server(&client), SidAuthOutcome::Authenticated(_)),
            "a peer that started strictly before `established` must be authenticated"
        );
    }

    drop(server);
}

/// Both pin paths (`SO_PEERPIDFD` where the kernel has it, `pidfd_open`
/// otherwise) prove the SAME live child, forced via `pin_peer_for_test`
/// so this exercises both branches deterministically regardless of what
/// this kernel actually supports (GitHub runners may have
/// `SO_PEERPIDFD`; the backend hosts, kernel 5.15, do not -- on a kernel
/// without it the `prefer_peerpidfd: true` path must FALL THROUGH to
/// `pidfd_open` and still prove, never `Undetermined` for that alone).
#[test]
fn both_pin_paths_prove_a_live_child() {
    if !run_isolated("both_pin_paths_prove_a_live_child") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");

    // DEVIATION FROM THE BRIEF'S LITERAL "socketpair" WORDING, reported
    // here: a `socketpair(2)`'s `SO_PEERCRED`/`SO_PEERPIDFD` identity is
    // latched to whichever process called `socketpair()` -- fixed at
    // CREATION time -- and handing one end to an already-forked child via
    // `fork`/`exec` never changes that; there is no way to make an
    // unrelated already-running `sleep` process become a socketpair's
    // reported peer after the fact. The closest correct construction that
    // keeps the actual property under test (a REAL live child whose pidfd
    // this kernel's own `SO_PEERPIDFD` genuinely resolves to, exercised
    // exactly like a real attacker or a real supervisor would observe it)
    // is a `connect(2)`-based pair instead: the CHILD itself calls
    // `connect()` -- via `pre_exec`, on a raw, non-CLOEXEC fd so it
    // SURVIVES the subsequent `execve` into `sleep` -- which is precisely
    // what latches the CHILD's own (exec-stable) pid as the peer identity
    // on the PARENT's accepted side, on every kernel this crate targets.
    use std::os::unix::process::CommandExt;
    let path_bytes = path.as_os_str().as_bytes().to_vec();
    let mut command = std::process::Command::new("sleep");
    command.arg("30");
    // SAFETY: `connect_raw_surviving_exec` only issues `socket`/`connect`
    // syscalls (no allocation, no locking) between `fork` and `exec`, the
    // async-signal-safety contract `pre_exec` requires.
    unsafe {
        command.pre_exec(move || connect_raw_surviving_exec(&path_bytes));
    }
    let mut child = command.spawn().expect("spawn sleep 30 (pre_exec: connect)");
    let pid = child.id();

    struct KillGuard<'a>(&'a mut std::process::Child);
    impl Drop for KillGuard<'_> {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _guard = KillGuard(&mut child);

    let (accepted, _addr) = listener.accept().expect("accept the child's own connect()");

    let start = challenge_unix::process_start_ticks(pid).expect("process_start_ticks(child)");
    let creds = PeerCredentials {
        pid,
        uid: unsafe { libc::geteuid() },
        gid: unsafe { libc::getegid() },
    };
    let established = start + 1; // strictly after the child's own start time

    for prefer_peerpidfd in [true, false] {
        use std::os::fd::AsRawFd;
        let outcome = challenge_unix::pin_peer_for_test(
            accepted.as_raw_fd(),
            &creds,
            established,
            prefer_peerpidfd,
        );
        match outcome {
            ChallengeOutcome::Proven((_pidfd, proven_start)) => {
                assert_eq!(proven_start, start, "prefer_peerpidfd={prefer_peerpidfd}");
            }
            other => panic!("prefer_peerpidfd={prefer_peerpidfd}: expected Proven, got {other:?}"),
        }
    }
}

/// Runs in the forked child, before `execve` replaces it with `sleep`:
/// a raw, deliberately non-`CLOEXEC` `connect()` to `path_bytes`, so the
/// resulting fd survives into the exec'd `sleep` process and stays open
/// for its whole 30s lifetime -- `execve` never changes a process's pid,
/// so the connection this establishes keeps identifying the SAME,
/// exec-stable pid the parent already has from `Command::id()`.
/// Syscall-only (no allocation, no libc calls beyond `socket`/`connect`)
/// to stay within `pre_exec`'s async-signal-safety contract.
unsafe fn connect_raw_surviving_exec(path_bytes: &[u8]) -> std::io::Result<()> {
    let raw = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut addr: libc::sockaddr_un = std::mem::zeroed();
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (dst, &b) in addr.sun_path.iter_mut().zip(path_bytes) {
        *dst = b as libc::c_char;
    }
    let addr_len =
        (std::mem::size_of::<libc::sa_family_t>() + path_bytes.len() + 1) as libc::socklen_t;
    let rc = libc::connect(raw, std::ptr::addr_of!(addr).cast(), addr_len);
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Deliberately leaked: `raw` must stay open (not closed, not
    // CLOEXEC'd) so `sleep` inherits it across the exec this closure
    // precedes.
    Ok(())
}

/// The full lifecycle a proof's retained pidfd supports, exercised
/// through `ChallengedProcess`'s own REAL public methods (via the
/// test-support `from_pinned_for_test` constructor, since a `sleep`
/// child cannot itself speak the wire protocol a full `challenge()` would
/// need): `wait` reports `false` while the child is alive, `terminate`
/// kills it, `wait` then reports `true`, and
/// `exit_status_after_confirmed_exit` is `Ok(_)` either way (kernel-
/// dependent whether the exit code is actually available -- `PIDFD_GET_INFO`
/// is 6.15+ -- but the CALL itself must never error once death is
/// confirmed).
#[test]
fn terminate_then_wait_then_exit_status_is_some_or_none_by_kernel() {
    if !run_isolated("terminate_then_wait_then_exit_status_is_some_or_none_by_kernel") {
        return;
    }
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep 30");
    let pid = child.id();
    let start = challenge_unix::process_start_ticks(pid).expect("process_start_ticks(child)");
    let creds = PeerCredentials {
        pid,
        uid: unsafe { libc::geteuid() },
        gid: unsafe { libc::getegid() },
    };
    let established = start + 1;

    let outcome = challenge_unix::pin_peer_for_test(0, &creds, established, false);
    let (pidfd, proven_start) = match outcome {
        ChallengeOutcome::Proven(v) => v,
        other => panic!("expected Proven, got {other:?}"),
    };
    assert_eq!(proven_start, start);

    let proof = ChallengedProcess::from_pinned_for_test(pidfd, pid, proven_start);

    assert!(
        !proof.wait(Duration::from_millis(100)).unwrap(),
        "the child must still be alive before terminate()"
    );

    proof.terminate().expect("terminate");

    assert!(
        proof.wait(Duration::from_secs(5)).unwrap(),
        "expected the pidfd to become readable (process exited) within 5s"
    );
    let _ = child.wait(); // reap the zombie

    match proof.exit_status_after_confirmed_exit() {
        Ok(Some(code)) => println!("PIDFD_GET_INFO reported exit_code={code}"),
        Ok(None) => println!(
            "PIDFD_GET_INFO unavailable or did not report PIDFD_INFO_EXIT on this kernel; exit status is None"
        ),
        Err(e) => panic!("exit_status_after_confirmed_exit must be Ok once death is confirmed, got {e}"),
    }
}
