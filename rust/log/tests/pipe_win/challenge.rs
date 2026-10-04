//! Same-connection challenge and SID authentication enforcement tests.

use super::*;


// ---------------------------------------------------------------------
// ADR 0041 U0 round-1: the same-connection challenge's real-pipe tests
// (moved here from `src/identity/challenge.rs`'s own unit tests -- security-
// sensitive Windows I/O belongs in a named integration target the
// windows-2022 job runs, not in the crate's own `--lib` test binary).
// Deadlines are GENEROUS (30s), not the tight 5s the original in-crate
// tests used, so two-core CI contention can't flake a genuinely correct
// challenge into a spurious `Undetermined` (round-1 review, "replace
// independent real deadlines with generous ones").
// ---------------------------------------------------------------------

fn self_pid_and_created() -> (u32, u64) {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, GetProcessTimes};
    unsafe {
        let pid = GetCurrentProcessId();
        let mut creation: FILETIME = std::mem::zeroed();
        let mut exit: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        assert_ne!(GetProcessTimes(GetCurrentProcess(), &mut creation, &mut exit, &mut kernel, &mut user), 0);
        let created = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        (pid, created)
    }
}

/// The `status` request has no body, so its ENCODED length alone is what
/// we wait for; the pipe is byte-type, so a single write is not
/// guaranteed to surface as a single `Bytes` event.
fn await_status_request(server: &PipeServer, conn_id: ConnId, timeout: Duration) {
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

/// Real challenge, real pipe, SAME process on both ends — a genuine
/// same-user server, proven. Also this test's own shared "give me a real
/// proven process" helper for the two tests below it.
fn self_proven_challenge() -> ChallengeOutcome<sot_log::identity::challenge_win::ChallengedProcess> {
    let voyage_id = fresh_voyage_id();
    let server = PipeServer::bind(&voyage_id, 1).expect("bind");
    let client = connect_voyage_pipe(&voyage_id).expect("connect");

    std::thread::scope(|scope| {
        let challenge_handle = scope.spawn(|| {
            let mut exchange = VoyageMgmtExchange::default();
            challenge(&client, &mut exchange, Instant::now() + Duration::from_secs(30))
        });

        let conn_id = expect_accepted(&server, TIMEOUT);
        await_status_request(&server, conn_id, TIMEOUT);
        let (pid, created) = self_pid_and_created();
        let reply = wire::encode_mgmt_reply(&MgmtReply::StatusOk { pid, created, survival: Survival::Normal }).unwrap();
        server.send(conn_id, reply, None).expect("send status_ok");

        challenge_handle.join().expect("challenge thread panicked")
    })
}

#[test]
fn challenge_proves_a_genuine_same_user_server() {
    if !run_isolated("challenge_proves_a_genuine_same_user_server") {
        return;
    }
    let (pid, created) = self_pid_and_created();
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
    let ChallengeOutcome::Proven(p) = self_proven_challenge() else {
        panic!("expected Proven")
    };
    assert!(p.reverify().unwrap());
    // Still running: this handle names our OWN test process.
    assert!(!p.wait(Duration::from_millis(50)).unwrap());
}

#[test]
fn challenge_rejects_a_pid_creation_mismatch_as_foreign() {
    if !run_isolated("challenge_rejects_a_pid_creation_mismatch_as_foreign") {
        return;
    }
    let voyage_id = fresh_voyage_id();
    let server = PipeServer::bind(&voyage_id, 1).expect("bind");
    let client = connect_voyage_pipe(&voyage_id).expect("connect");

    let outcome = std::thread::scope(|scope| {
        let challenge_handle = scope.spawn(|| {
            let mut exchange = VoyageMgmtExchange::default();
            challenge(&client, &mut exchange, Instant::now() + Duration::from_secs(30))
        });

        let conn_id = expect_accepted(&server, TIMEOUT);
        await_status_request(&server, conn_id, TIMEOUT);
        // A well-formed status_ok, but a FABRICATED pid/creation that
        // does not match the real server process (this test binary
        // itself) — the SID check upstream cannot catch this: same
        // account, wrong reply.
        let reply = wire::encode_mgmt_reply(&MgmtReply::StatusOk { pid: 1, created: 0, survival: Survival::Normal }).unwrap();
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
/// fires and `cancel()` unblocks it. Deterministic despite the short
/// deadline: the SERVER thread never does anything that could race it
/// (no reply, no close), so the only way this test can reach
/// `Undetermined` is via the cancellation path being exercised for real.
#[test]
fn challenge_cancels_a_genuinely_pending_read_when_the_deadline_expires() {
    if !run_isolated("challenge_cancels_a_genuinely_pending_read_when_the_deadline_expires") {
        return;
    }
    let voyage_id = fresh_voyage_id();
    let server = PipeServer::bind(&voyage_id, 1).expect("bind");
    let client = connect_voyage_pipe(&voyage_id).expect("connect");

    let outcome = std::thread::scope(|scope| {
        let challenge_handle = scope.spawn(|| {
            let mut exchange = VoyageMgmtExchange::default();
            // A short but real deadline -- the server below never replies,
            // so this can only resolve via the watchdog's own cancel.
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
    let voyage_id = fresh_voyage_id();
    let server = PipeServer::bind(&voyage_id, 1).expect("bind");
    let client = connect_voyage_pipe(&voyage_id).expect("connect");

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
/// below: binds a real named pipe server for the voyage id named by
/// `PIPE_WIN_XPROC_VOYAGE_ID`, answers exactly one `status` request with
/// THIS PROCESS's own real pid/creation time, then exits. A normal test
/// pass never sets that env var, so this is a silent no-op then — the
/// parent test is the only thing that ever invokes this BY NAME with it
/// set, in a dedicated child process.
#[test]
fn cross_process_challenge_server_role() {
    let Ok(voyage_id) = std::env::var("PIPE_WIN_XPROC_VOYAGE_ID") else {
        return;
    };
    let server = PipeServer::bind(&voyage_id, 1).expect("server role: bind");
    let conn_id = expect_accepted(&server, Duration::from_secs(30));
    await_status_request(&server, conn_id, Duration::from_secs(30));
    let (pid, created) = self_pid_and_created();
    let reply = wire::encode_mgmt_reply(&MgmtReply::StatusOk { pid, created, survival: Survival::Normal }).unwrap();
    server.send(conn_id, reply, None).expect("server role: send status_ok");
    // Stay alive until the CLIENT has read the reply and closed: `send` only
    // queues the bytes, and this process's exit drops the `PipeServer`, whose
    // teardown `DisconnectNamedPipe`s the instance — which DISCARDS anything
    // the client has not yet read. On a fast box the client always won that
    // race; on a loaded CI runner it lost it and the parent saw EOF instead
    // of `status_ok`, i.e. `Undetermined` (main, windows-latest, 2026-09-08).
    // The parent drops its client right after the challenge, so `Closed`
    // arrives promptly; the bound only guards a parent that died mid-test.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match next_event(&server, left) {
            LaneEvent::Closed(id, _) if id == conn_id => break,
            _ => continue,
        }
    }
}

/// The real cross-process test (ADR 0041 U0 round-1 required test):
/// `GetNamedPipeServerProcessId`, called on the CLIENT's own handle
/// (this process), must resolve to a GENUINELY DIFFERENT process's real
/// pid — closing the Microsoft-docs ambiguity Codex flagged (the API's
/// own parameter prose describes a handle from `CreateNamedPipe`, not a
/// client's `CreateFile` handle; Chromium's own client-side use is the
/// only precedent, not a documented guarantee). Every same-process test
/// above proves the PROTOCOL; only this one proves the OS call actually
/// crosses a real process boundary the way `challenge()` depends on.
#[test]
fn cross_process_challenge_proves_a_real_child_server() {
    if !run_isolated("cross_process_challenge_proves_a_real_child_server") {
        return;
    }
    let voyage_id = fresh_voyage_id();
    let exe = std::env::current_exe().expect("current_exe");
    let child = std::process::Command::new(&exe)
        .arg("--exact")
        .arg("cross_process_challenge_server_role")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env("PIPE_WIN_XPROC_VOYAGE_ID", &voyage_id)
        .env_remove("PIPE_WIN_TEST_CHILD")
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

    // ADR 0043 decision 27: `connect_voyage_pipe` no longer retries an
    // absent pipe (`ERROR_FILE_NOT_FOUND` now fails on the first
    // attempt) -- this test owns the ordinary race of the child not
    // having bound yet via `wait_for_endpoint`.
    let client = wait_for_endpoint(|| connect_voyage_pipe(&voyage_id), Duration::from_secs(30));
    let mut exchange = VoyageMgmtExchange::default();
    let outcome = challenge(&client, &mut exchange, Instant::now() + Duration::from_secs(30));

    match outcome {
        ChallengeOutcome::Proven(p) => {
            assert_eq!(p.pid(), child_pid, "the challenged pid must be the REAL CHILD's, not our own");
            assert_ne!(p.pid(), std::process::id(), "a same-process pid here would prove nothing cross-process");
        }
        other => panic!("expected Proven against a real cross-process server, got {other:?}"),
    }

    // Close our end FIRST: the child waits for our `Closed` before it exits
    // (see `cross_process_challenge_server_role`), so reaping it while this
    // handle is still open would deadlock. Then reap it normally and defuse
    // the guard's own kill (a no-op by then, kept only for the panic/
    // early-return paths above).
    drop(client);
    if let Some(c) = guard.0.as_mut() {
        let _ = c.wait();
    }
    guard.0 = None;
}

// ---------------------------------------------------------------------
// U1a: SID authentication enforcement in the shared `connect_voyage_pipe`
// constructor. Every step-5 client — mgmt or attach, tests and the e2e
// harness included — now gets steps 1-3 of ADR 0041's same-connection
// challenge (`challenge_win::authenticate_server`) as PART of connecting, not
// as a separate call the caller has to remember to make. This is
// DELIBERATELY WEAKER than the full five-step `challenge()` (Codex round-1
// Blocker 1): no reply round trip happens here, so no liveness/pid-binding
// proof is claimed at this layer — see `connect_voyage_pipe`'s own doc for
// the full reasoning and why the attach lane's `hello` still gets to be
// the connection's first frame.
// ---------------------------------------------------------------------

/// The pass case: against a genuine same-account server,
/// `connect_voyage_pipe`'s SID authentication is a transparent
/// pass-through — the connection it hands back is fully usable for the
/// caller's own intended protocol (an ordinary mgmt round trip here), not
/// merely "connected". This exercises the ENFORCED CONSTRUCTOR ITSELF
/// (`connect_voyage_pipe`, not `authenticate_server` or `challenge`
/// directly), so a regression that stopped calling `authenticate_server`
/// at all would still pass this test (nothing here proves enforcement
/// happened) -- that is exactly why the failure-mapping unit tests in
/// `lane/pipe_win/` itself (`map_peer_auth_outcome`) exist alongside it: they
/// prove the CONSTRUCTOR's mapping logic in isolation, and this test
/// proves the happy path stays usable end to end.
#[test]
fn connect_voyage_pipe_peer_authentication_enforced_pass_against_a_genuine_server() {
    if !run_isolated("connect_voyage_pipe_peer_authentication_enforced_pass_against_a_genuine_server") {
        return;
    }
    let voyage_id = fresh_voyage_id();
    let server = PipeServer::bind(&voyage_id, 1).expect("bind");
    let client = connect_voyage_pipe(&voyage_id)
        .expect("SID-authentication-enforced connect must pass against a genuine same-account server");

    let conn_id = expect_accepted(&server, TIMEOUT);
    let probe = wire::encode_mgmt_request(&MgmtRequest::Probe).unwrap();
    client.write_all(&probe).expect("the connection must still be fully usable after authentication");
    let expected = wire::encode_mgmt_request(&MgmtRequest::Probe).unwrap();
    let got = accumulate_bytes(&server, conn_id, expected.len(), TIMEOUT);
    assert_eq!(got, expected);

    let reply = wire::encode_mgmt_reply(&MgmtReply::ProbeOk).unwrap();
    server.send(conn_id, reply.clone(), None).expect("send probe_ok");
    let mut buf = [0u8; 512];
    let n = client.read(&mut buf).expect("read probe_ok");
    assert_eq!(&buf[..n], reply.as_slice());
}

/// A `ChallengeableConnection` whose `raw_handle()` is deliberately
/// invalid (`INVALID_HANDLE_VALUE`, the exact value a failed `CreateFileW`
/// itself would have produced) — proves `authenticate_server`'s own
/// typed-failure surface with NO real pipe, process, or account boundary
/// needed: step 1 (`GetNamedPipeServerProcessId`) itself fails against
/// this handle, deterministically, on every run. `write_all`/`read`/
/// `cancel` are never reached (`authenticate_server` never touches
/// `IdentityExchange`, unlike the full `challenge()`) — `unreachable!()`
/// makes that a loud, checked assumption rather than a silent one.
///
/// A genuine WRONG-SID server (an other-user process winning the pipe
/// name first) is the ADR's own step-7 REAL-MACHINE acceptance row ("on a
/// real Windows machine, an OTHER-USER process pre-binds... every public
/// client entry point classifies it FOREIGN") — not constructible in CI
/// without a second real account, so it is deliberately not attempted
/// here; this test instead proves the OTHER typed failure — `Undetermined`
/// — that steps 1-3 alone can already produce.
struct InvalidHandleConn;
impl sot_log::identity::challenge::ChallengeableConnection for InvalidHandleConn {
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
/// L1-unix LU1a: raw-handle access moved off `ChallengeableConnection`
/// onto this separate, Windows-only extension trait — see
/// `challenge_win.rs`'s own doc for why.
impl sot_log::identity::challenge_win::PipeChallengeable for InvalidHandleConn {
    fn raw_handle(&self) -> windows_sys::Win32::Foundation::HANDLE {
        windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE
    }
}

#[test]
fn authenticate_server_is_undetermined_when_step_one_itself_fails() {
    use sot_log::identity::challenge::PeerAuthOutcome;
    use sot_log::identity::challenge_win::authenticate_server;
    let outcome = authenticate_server(&InvalidHandleConn);
    assert!(matches!(outcome, PeerAuthOutcome::Undetermined), "{outcome:?}");
}
