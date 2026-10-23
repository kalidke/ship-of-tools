#![cfg(target_os = "linux")]
//! The Linux identity challenge (`sot_platform::identity::challenge_unix`) and the `SocketClient` it authenticates,
//! driven against the row socket: a real `RowServer` over a fake `RowRun` answers the hello, and the adversarial cases
//! are made by the exchange the client drives (`support/row_challenge.rs`), since the row answers its own hello.
//! Each case runs in its own process (`sot_platform::test_isolated::run_isolated`).

#[path = "support/row_challenge.rs"]
mod row_challenge;

use row_challenge::{Lying, NotAFrame, Unanswered};
use sot_log::lane::connect_own::connect_own;
use sot_log::lane::socket_unix::SocketClient;
use sot_log::row::{socket_path_in, RowHelloExchange, RowRun, RowServer};
use sot_platform::identity::challenge::{ChallengeOutcome, ChallengeableConnection, PeerAuthOutcome};
use sot_platform::identity::challenge_unix::{
    self, authenticate_server, challenge, self_start_ticks, ChallengedProcess, PeerCredentials, SocketChallengeable,
};
use sot_platform::identity::exchange::{ExchangeDecode, IdentityExchange};
use sot_platform::test_isolated::run_isolated;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// A per-event bound used throughout (well inside `sot_platform::test_isolated::ISOLATION_TIMEOUT`, so
/// a stalled event always trips before the parent's own kill fires).
const TIMEOUT: Duration = Duration::from_secs(10);

/// A fresh mode-0700 folder under `/tmp` (the row's folder must be private, and `$TMPDIR` may overflow `sun_path`).
fn private_tmp() -> tempfile::TempDir {
    let tmp = tempfile::Builder::new().prefix("sot-t").tempdir_in("/tmp").expect("tempdir under /tmp");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    tmp
}

fn row_path(dir: &Path) -> PathBuf {
    socket_path_in(dir, "0123456789abcdef").expect("a short path fits")
}

/// A row server over a fake run, and the folder it lives in.
fn bind_row() -> (tempfile::TempDir, PathBuf, RowServer) {
    let dir = private_tmp();
    let path = row_path(dir.path());
    let server = RowServer::bind(&path, RowRun::new("run-one", "ready")).expect("bind");
    (dir, path, server)
}

/// The pin requires the peer's observed start time to be STRICTLY before the connect's anchor (a tie is
/// `Undetermined` by design). In a same-process self-connect test, both are measured within one process's own brief
/// lifetime, both quantized to `sysconf(_SC_CLK_TCK)` (10 ms on this host), so a `run_isolated` child that binds and
/// connects almost immediately after starting can land on the same tick. A real deployment never hits this: the
/// server process already exists, with an earlier start time, long before any client dials it. This sleep is the
/// test-only accommodation for the "peer is myself, freshly started" topology these self-tests use, never a change to
/// the production pin, which stays exactly as strict as the ADR requires.
fn ensure_established_gap() {
    std::thread::sleep(Duration::from_millis(50));
}

/// Real challenge, real socket, SAME process on both ends: a genuine same-user server, proven. Also this file's shared
/// "give me a real proven process" helper for the test below it.
fn self_proven_challenge() -> (ChallengeOutcome<ChallengedProcess>, tempfile::TempDir, RowServer) {
    let (dir, path, server) = bind_row();
    ensure_established_gap();
    let client = connect_own(&path).expect("connect");
    let mut exchange = RowHelloExchange::new("test");
    let outcome = challenge(&client, &mut exchange, Instant::now() + Duration::from_secs(30));
    (outcome, dir, server)
}

#[test]
fn challenge_proves_a_genuine_same_user_server() {
    if !run_isolated("challenge_proves_a_genuine_same_user_server") {
        return;
    }
    let pid = std::process::id();
    let created = self_start_ticks().expect("self_start_ticks");
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
    if !run_isolated("challenged_process_reverify_and_wait_reflect_a_live_self_proof") {
        return;
    }
    let (ChallengeOutcome::Proven(p), _dir, _server) = self_proven_challenge() else {
        panic!("expected Proven")
    };
    assert!(p.reverify().unwrap());
    // Still running: this pidfd names our OWN test process.
    assert!(!p.wait(Duration::from_millis(50)).unwrap());
}

/// Same account, wrong reply: a pid and creation that are not the server process's are `Foreign`, which the account
/// check upstream cannot catch.
#[test]
fn challenge_rejects_a_pid_creation_mismatch_as_foreign() {
    if !run_isolated("challenge_rejects_a_pid_creation_mismatch_as_foreign") {
        return;
    }
    let (_dir, path, _server) = bind_row();
    ensure_established_gap();
    let client = connect_own(&path).expect("connect");
    let outcome = challenge(&client, &mut Lying::new(1, 0), Instant::now() + Duration::from_secs(30));
    assert!(matches!(outcome, ChallengeOutcome::Foreign), "{outcome:?}");
}

/// Real deadline expiry, real pending `read`, real `conn.cancel()`, as opposed to the ordered EOF of the next test (a
/// different path entirely). The row waits for the rest of a half frame and answers nothing, and never closes: the
/// client's `read` is genuinely blocked until the watchdog's deadline fires and `cancel()` unblocks it.
#[test]
fn challenge_cancels_a_genuinely_pending_read_when_the_deadline_expires() {
    if !run_isolated("challenge_cancels_a_genuinely_pending_read_when_the_deadline_expires") {
        return;
    }
    let (_dir, path, _server) = bind_row();
    ensure_established_gap();
    let client = connect_own(&path).expect("connect");
    // A short but real deadline: the row never replies, so this can only resolve via the watchdog's own cancel.
    let outcome = challenge(&client, &mut Unanswered, Instant::now() + Duration::from_millis(300));
    assert!(matches!(outcome, ChallengeOutcome::Undetermined), "{outcome:?}");
}

#[test]
fn challenge_classifies_connection_death_mid_challenge_as_undetermined() {
    if !run_isolated("challenge_classifies_connection_death_mid_challenge_as_undetermined") {
        return;
    }
    let (_dir, path, _server) = bind_row();
    ensure_established_gap();
    let client = connect_own(&path).expect("connect");
    // The row ends the connection on a first line that is no frame, without ever answering.
    let outcome = challenge(&client, &mut NotAFrame, Instant::now() + Duration::from_secs(30));
    assert!(matches!(outcome, ChallengeOutcome::Undetermined), "{outcome:?}");
}

/// The "child" half of `cross_process_challenge_proves_a_real_child_server` below: binds a real row server at the
/// socket named by `CHALLENGE_UNIX_XPROC_SOCKET`, which answers the hello with THIS PROCESS's own real pid and
/// creation time, then stays up (exiting only once the parent kills it), so the parent can assert on its still-live
/// pid via `SO_PEERCRED` without a reap race. A normal test pass never sets that env var, so this is a silent no-op.
#[test]
fn cross_process_challenge_server_role() {
    let Ok(path) = std::env::var("CHALLENGE_UNIX_XPROC_SOCKET") else {
        return;
    };
    sot_platform::test_isolated::enter("cross_process_challenge_server_role");
    let created = self_start_ticks().expect("self_start_ticks");
    // Don't bind (hence don't become connectable) until this process's OWN clock has advanced strictly past its own
    // start tick: the parent's connect anchor is sampled essentially at the moment its `connect()` succeeds, which can
    // only happen once THIS bind has run, so the anchor lands strictly after `created` instead of risking the
    // tick-quantization tie the self-connect tests hit (see `ensure_established_gap`). Spinning on the OBSERVED tick,
    // not a fixed sleep, never over- or under-shoots whatever this kernel's `CLK_TCK` is.
    while challenge_unix::boot_ticks_now().expect("boot_ticks_now") <= created {
        std::thread::sleep(Duration::from_millis(1));
    }
    let _server = RowServer::bind(Path::new(&path), RowRun::new("run-one", "ready")).expect("server role: bind");
    // Announce readiness only now that the tick-wait AND a successful bind have happened, so the parent starts its
    // own connect after both. A LEADING newline matters: the harness has already written "test <name> ... " to this
    // stdout with no trailing newline, and a bare `println!("ready")` would land on that same line, never matching.
    println!("\nready");
    std::io::stdout().flush().expect("flush the ready line");
    // Stay up: the parent asserts against this process's pid then kills it via its own `KillGuard`.
    std::thread::sleep(Duration::from_secs(30));
}

/// The real cross-process test (ADR 0043 decision 8's acceptance row): `SO_PEERCRED`, read via the CLIENT's own fd,
/// must resolve to a GENUINELY DIFFERENT process's real pid, the kernel-level fact the design depends on.
#[test]
fn cross_process_challenge_proves_a_real_child_server() {
    if !run_isolated("cross_process_challenge_proves_a_real_child_server") {
        return;
    }
    let dir = private_tmp();
    let path = row_path(dir.path());
    let (mut role, role_entry) = sot_platform::test_isolated::test_command("cross_process_challenge_server_role");
    let mut child = role
        .env("CHALLENGE_UNIX_XPROC_SOCKET", &path)
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn the cross-process server child");
    let child_pid = child.id();
    let child_stdout = child.stdout.take().expect("piped stdout");

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

    // Wait for the child's own "ready" line (printed only after ITS tick-wait AND a successful bind) BEFORE starting
    // our connect, so the parent's own pre-connect anchor cannot be sampled before the child has bound at all. Bounded
    // via a helper thread + channel (10s, loud on expiry) rather than a bare blocking `read_line`, which could hang
    // forever against a wedged child. Any OTHER line the harness prints first is skipped.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let reader_thread = std::thread::spawn(move || {
        let mut reader = BufReader::new(child_stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    let _ = ready_tx.send(Err("child stdout closed before printing its own \"ready\" line".to_string()));
                    return;
                }
                Ok(_) => {
                    if line.trim() == "ready" {
                        let _ = ready_tx.send(Ok(()));
                        return;
                    }
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("reading the child's own readiness line: {e}")));
                    return;
                }
            }
        }
    });
    match ready_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => {}
        Ok(Err(msg)) => panic!("{msg}"),
        Err(_) => panic!("timed out after 10s waiting for the child's own \"ready\" line"),
    }
    role_entry.assert_once(child_pid);
    reader_thread.join().expect("readiness reader thread panicked");

    let client = connect_own(&path).expect("connect to the cross-process server");
    let mut exchange = RowHelloExchange::new("test");
    let outcome = challenge(&client, &mut exchange, Instant::now() + Duration::from_secs(30));

    match outcome {
        ChallengeOutcome::Proven(p) => {
            assert_eq!(p.pid(), child_pid, "the challenged pid must be the REAL CHILD's, not our own");
            assert_ne!(p.pid(), std::process::id(), "a same-process pid here would prove nothing cross-process");
        }
        other => panic!("expected Proven against a real cross-process server, got {other:?}"),
    }

    if let Some(c) = guard.0.as_mut() {
        let _ = c.kill();
        let _ = c.wait();
    }
    guard.0 = None;
}

/// The pass case: against a genuine same-account row, `authenticate_server` is a transparent pass-through. The
/// connection stays fully usable for the caller's own protocol (the hello round trip here), not merely "connected".
#[test]
fn authenticate_server_passes_a_genuine_row_and_leaves_the_connection_usable() {
    if !run_isolated("authenticate_server_passes_a_genuine_row_and_leaves_the_connection_usable") {
        return;
    }
    let (_dir, path, _server) = bind_row();
    ensure_established_gap();
    let client = connect_own(&path).expect("connect");
    match authenticate_server(&client) {
        PeerAuthOutcome::Authenticated(peer) => assert_eq!(peer.pid, std::process::id()),
        other => panic!("authentication must pass against a genuine same-account server, got {other:?}"),
    }
    let mut hello = RowHelloExchange::new("test");
    client.write_all(&hello.encode_request()).expect("the connection must still be fully usable after authentication");
    let named = sot_platform::test_wait::within(TIMEOUT, "the hello reply", move || {
        let mut buf = [0u8; 4096];
        let mut got = Vec::new();
        loop {
            let n = client.read(&mut buf).expect("read the hello reply");
            assert!(n > 0, "the row closed before it answered");
            got.extend_from_slice(&buf[..n]);
            match hello.feed(&got[got.len() - n..]) {
                ExchangeDecode::Incomplete => continue,
                ExchangeDecode::Identity { pid, .. } => break Some(pid),
                ExchangeDecode::Foreign => break None,
            }
        }
    });
    assert_eq!(named, Some(std::process::id()), "the row's hello names this process");
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
    assert!(matches!(outcome, PeerAuthOutcome::Undetermined), "{outcome:?}");
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
    let (_dir, path, server) = bind_row();
    let own_start = self_start_ticks().expect("self_start_ticks");

    {
        let stream = std::os::unix::net::UnixStream::connect(&path).unwrap();
        let client = SocketClient::from_stream_for_test(stream, own_start);
        assert!(
            matches!(authenticate_server(&client), PeerAuthOutcome::Undetermined),
            "a tie between the peer's start time and `established` must be Undetermined, never Proven"
        );
    }
    {
        let stream = std::os::unix::net::UnixStream::connect(&path).unwrap();
        let client = SocketClient::from_stream_for_test(stream, own_start + 1);
        assert!(
            matches!(authenticate_server(&client), PeerAuthOutcome::Authenticated(_)),
            "a peer that started strictly before `established` must be authenticated"
        );
    }

    server.shutdown();
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
    let dir = private_tmp();
    let path = row_path(dir.path());
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

/// The lifecycle a proof's retained pidfd supports, exercised through
/// `ChallengedProcess`'s own REAL public methods (via the test-support
/// `from_pinned_for_test` constructor, since a `sleep` child cannot
/// itself speak the wire protocol a full `challenge()` would need):
/// `wait` reports `false` while the child is alive, `terminate` kills
/// it, and `wait` then reports `true` -- death is CONFIRMED. ADR 0043
/// decision 33: the challenged-process exit-status accessor this test
/// used to exercise past that point (`exit_status_after_confirmed_exit`,
/// `PIDFD_GET_INFO`) is deleted outright (readerless), so this stops at
/// the confirmed-death proof.
#[test]
fn terminate_then_wait_confirms_death() {
    if !run_isolated("terminate_then_wait_confirms_death") {
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
}
