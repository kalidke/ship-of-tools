//! Same-connection challenge and SID authentication enforcement tests, against the row pipe: a real `RowServer` over a
//! fake `RowRun` answers the hello, and the adversarial cases are made by the exchange the client drives
//! (`support/row_challenge.rs`), since the row answers its own hello.

use super::row_challenge::{Lying, NotAFrame, Unanswered};
use super::*;
use sot_platform::transport::connect_own::connect_own;
use sot_log::row::{RowHelloExchange, RowRun, RowServer};
use sot_platform::identity::challenge::ChallengeOutcome;
use sot_platform::identity::challenge_win::challenge;

// Deadlines are GENEROUS (30s), not tight, so two-core CI contention can't flake a genuinely correct challenge into a
// spurious `Undetermined`.

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

/// Real challenge, real pipe, SAME process on both ends: a genuine same-user server, proven. Also this file's shared
/// "give me a real proven process" helper for the test below it.
fn self_proven_challenge() -> (ChallengeOutcome<sot_platform::identity::challenge_win::ChallengedProcess>, RowServer) {
    let (path, server, _run) = bind_row();
    let client = connect_own(&path).expect("connect");
    let mut exchange = RowHelloExchange::new("test");
    let outcome = challenge(&client, &mut exchange, Instant::now() + Duration::from_secs(30));
    (outcome, server)
}

#[test]
fn challenge_proves_a_genuine_same_user_server() {
    if !run_isolated("challenge::challenge_proves_a_genuine_same_user_server") {
        return;
    }
    let (pid, created) = self_pid_and_created();
    match self_proven_challenge().0 {
        ChallengeOutcome::Proven(p) => {
            assert_eq!(p.pid(), pid);
            assert_eq!(p.created(), created);
        }
        other => panic!("expected Proven, got {other:?}"),
    }
}

#[test]
fn challenged_process_reverify_and_wait_reflect_a_live_self_proof() {
    if !run_isolated("challenge::challenged_process_reverify_and_wait_reflect_a_live_self_proof") {
        return;
    }
    let (ChallengeOutcome::Proven(p), _server) = self_proven_challenge() else {
        panic!("expected Proven")
    };
    assert!(p.reverify().unwrap());
    // Still running: this handle names our OWN test process.
    assert!(!p.wait(Duration::from_millis(50)).unwrap());
}

/// Same account, wrong reply: a pid and creation that are not the server process's are `Foreign`, which the SID check
/// upstream cannot catch.
#[test]
fn challenge_rejects_a_pid_creation_mismatch_as_foreign() {
    if !run_isolated("challenge::challenge_rejects_a_pid_creation_mismatch_as_foreign") {
        return;
    }
    let (path, _server, _run) = bind_row();
    let client = connect_own(&path).expect("connect");
    let outcome = challenge(&client, &mut Lying::new(1, 0), Instant::now() + Duration::from_secs(30));
    assert!(matches!(outcome, ChallengeOutcome::Foreign), "{outcome:?}");
}

/// Real deadline expiry, real pending `read`, real `conn.cancel()`, as opposed to the ordered EOF of the next test (a
/// different path entirely). The row waits for the rest of a half frame (a pipe has no hello deadline) and answers
/// nothing, and never closes: the client's `read` is genuinely blocked until the watchdog's deadline fires and
/// `cancel()` unblocks it.
#[test]
fn challenge_cancels_a_genuinely_pending_read_when_the_deadline_expires() {
    if !run_isolated("challenge::challenge_cancels_a_genuinely_pending_read_when_the_deadline_expires") {
        return;
    }
    let (path, _server, _run) = bind_row();
    let client = connect_own(&path).expect("connect");
    // A short but real deadline: the row never replies and holds the connection open, so the challenge is still
    // pending until the watchdog's cancel ends it, well before the row's own 5 s hello timeout could close the stream.
    let started = Instant::now();
    let outcome = std::thread::scope(|scope| {
        let challenge_handle = scope.spawn(|| challenge(&client, &mut Unanswered, started + Duration::from_millis(300)));
        std::thread::sleep(Duration::from_millis(100));
        assert!(!challenge_handle.is_finished(), "the challenge ended before its deadline: the peer did not hold it pending");
        challenge_handle.join().expect("challenge thread panicked")
    });
    assert!(started.elapsed() < Duration::from_secs(3), "the cancel completed only after {:?}: the row's own timeout may have ended it", started.elapsed());
    assert!(matches!(outcome, ChallengeOutcome::Undetermined), "{outcome:?}");
}

#[test]
fn challenge_classifies_connection_death_mid_challenge_as_undetermined() {
    if !run_isolated("challenge::challenge_classifies_connection_death_mid_challenge_as_undetermined") {
        return;
    }
    let (path, _server, _run) = bind_row();
    let client = connect_own(&path).expect("connect");
    // The row ends the connection on a first line that is no frame, without ever answering.
    let outcome = challenge(&client, &mut NotAFrame, Instant::now() + Duration::from_secs(30));
    assert!(matches!(outcome, ChallengeOutcome::Undetermined), "{outcome:?}");
}

/// The "child" half of `cross_process_challenge_proves_a_real_child_server` below: binds a real row server on the
/// pipe named by `PIPE_WIN_XPROC_PIPE`, which answers the hello with THIS PROCESS's own real pid and creation time,
/// then stays up until the parent kills it. A normal test pass never sets that env var, so this is a silent no-op then:
/// the parent test is the only thing that ever invokes this BY NAME with it set, in a dedicated child process.
#[test]
fn cross_process_challenge_server_role() {
    let Ok(path) = std::env::var("PIPE_WIN_XPROC_PIPE") else {
        return;
    };
    sot_platform::test_isolated::enter("challenge::cross_process_challenge_server_role");
    let _server = RowServer::bind(std::path::Path::new(&path), RowRun::new("run-one", "ready")).expect("server role: bind");
    std::thread::sleep(Duration::from_secs(30));
}

/// The real cross-process test (ADR 0041 U0 round-1 required test): `GetNamedPipeServerProcessId`, called on the
/// CLIENT's own handle (this process), must resolve to a GENUINELY DIFFERENT process's real pid, closing the
/// Microsoft-docs ambiguity Codex flagged (the API's own parameter prose describes a handle from `CreateNamedPipe`, not
/// a client's `CreateFile` handle; Chromium's own client-side use is the only precedent, not a documented guarantee).
/// Every same-process test above proves the PROTOCOL; only this one proves the OS call actually crosses a real process
/// boundary the way `challenge()` depends on.
#[test]
fn cross_process_challenge_proves_a_real_child_server() {
    if !run_isolated("challenge::cross_process_challenge_proves_a_real_child_server") {
        return;
    }
    let path = fresh_row_pipe();
    let (mut role, role_entry) =
        sot_platform::test_isolated::test_command("challenge::cross_process_challenge_server_role");
    let child = role
        .env("PIPE_WIN_XPROC_PIPE", &path)
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
    let _guard = KillGuard(Some(child));

    // ADR 0043 decision 27: the connector no longer retries an absent pipe (`ERROR_FILE_NOT_FOUND` fails on the first
    // attempt): this test owns the ordinary race of the child not having bound yet via `wait_for_endpoint`.
    let client = wait_for_endpoint(|| connect_own(&path), Duration::from_secs(30));
    let mut exchange = RowHelloExchange::new("test");
    let outcome = challenge(&client, &mut exchange, Instant::now() + Duration::from_secs(30));

    match outcome {
        ChallengeOutcome::Proven(p) => {
            assert_eq!(p.pid(), child_pid, "the challenged pid must be the REAL CHILD's, not our own");
            assert_ne!(p.pid(), std::process::id(), "a same-process pid here would prove nothing cross-process");
        }
        other => panic!("expected Proven against a real cross-process server, got {other:?}"),
    }
    role_entry.assert_once(child_pid);
}

/// The pass case: against a genuine same-account row, `authenticate_server` is a transparent pass-through. The
/// connection stays fully usable for the caller's own protocol (the hello round trip here), not merely "connected".
/// This exercises the authentication on its own, not the full five-step `challenge()`: no reply round trip happens in
/// it, so no liveness or pid-binding proof is claimed at this layer.
#[test]
fn authenticate_server_passes_a_genuine_row_and_leaves_the_connection_usable() {
    use sot_platform::identity::challenge::PeerAuthOutcome;
    use sot_platform::identity::challenge_win::authenticate_server;
    use sot_platform::identity::exchange::{ExchangeDecode, IdentityExchange};
    if !run_isolated("challenge::authenticate_server_passes_a_genuine_row_and_leaves_the_connection_usable") {
        return;
    }
    let (path, _server, _run) = bind_row();
    let client = connect_own(&path).expect("connect");
    match authenticate_server(&client) {
        PeerAuthOutcome::Authenticated(peer) => assert_eq!(peer.pid, std::process::id()),
        other => panic!("authentication must pass against a genuine same-account server, got {other:?}"),
    }
    let mut hello = RowHelloExchange::new("test");
    client.write_all(&hello.encode_request()).expect("the connection must still be fully usable after authentication");
    let named = sot_platform::test_wait::within(TIMEOUT, "the hello reply", move || {
        let mut buf = [0u8; 4096];
        loop {
            let n = client.read(&mut buf).expect("read the hello reply");
            assert!(n > 0, "the row closed before it answered");
            match hello.feed(&buf[..n]) {
                ExchangeDecode::Incomplete => continue,
                ExchangeDecode::Identity { pid, .. } => break Some(pid),
                ExchangeDecode::Foreign => break None,
            }
        }
    });
    assert_eq!(named, Some(std::process::id()), "the row's hello names this process");
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
impl sot_platform::identity::challenge::ChallengeableConnection for InvalidHandleConn {
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
impl sot_platform::identity::challenge_win::PipeChallengeable for InvalidHandleConn {
    fn raw_handle(&self) -> windows_sys::Win32::Foundation::HANDLE {
        windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE
    }
}

#[test]
fn authenticate_server_is_undetermined_when_step_one_itself_fails() {
    use sot_platform::identity::challenge::PeerAuthOutcome;
    use sot_platform::identity::challenge_win::authenticate_server;
    let outcome = authenticate_server(&InvalidHandleConn);
    assert!(matches!(outcome, PeerAuthOutcome::Undetermined), "{outcome:?}");
}
