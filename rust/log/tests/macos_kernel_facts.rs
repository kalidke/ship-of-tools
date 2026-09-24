#![cfg(target_os = "macos")]
//! macOS kernel-fact regression tests — the OS behaviours the macOS lane
//! is blocked on, pinned here so that a future OS change reports itself as
//! a named red test rather than as a silent auth or capsule regression
//! months later.
//!
//! The whole file is `#![cfg(target_os = "macos")]`: it compiles away to
//! nothing everywhere else, so the blocking `macos-latest` CI job
//! (`cargo test --workspace --locked`) is the only place any of them runs.
//!
//! # Fact 1 — does the kernel serve the SERVER's identity to a CLIENT?
//!
//! `src/challenge_unix.rs` (Linux only, read its "Why `SO_PEERCRED` on the
//! CLIENT's own fd works" section) proves the peer's pid from the CLIENT's
//! OWN fd: `SO_PEERCRED` is latched at `connect(2)` onto BOTH ends of a
//! connected `AF_UNIX` socket, so a client reading its own socket learns
//! the real server pid — which is the entire foundation of
//! `authenticate_server()`. macOS has no `SO_PEERCRED`, and `getpeereid`
//! yields euid/egid only: no pid, and nothing that pins an identity
//! against pid reuse. The candidate replacement is
//! `getsockopt(SOL_LOCAL, LOCAL_PEERTOKEN)`, whose `audit_token_t` carries
//! both a pid (word 5) and a pidversion (word 7 — the reuse generation,
//! macOS's own answer to the `/proc/<pid>/stat` start-time pin
//! `challenge_unix.rs` uses).
//!
//! THE DIRECTION IS THE WHOLE POINT. A token that only resolves
//! server-side (the server learns its client) is not enough:
//! `authenticate_server` needs the CLIENT to learn the SERVER. So this
//! test uses two REAL processes joined by a real `connect(2)` — a
//! `socketpair` would prove nothing (no `connect` ever happens) and a
//! server thread inside the test process would prove nothing (same pid).
//! It ASSERTS the client's half and merely RECORDS the server's half,
//! which costs one extra syscall and tells us whether the mechanism works
//! at all in the case where the interesting direction does not.
//!
//! # Fact 2 — does closing a pty master reap the child on the slave side?
//!
//! The Linux capsule leans on `PR_SET_PDEATHSIG` (`src/producer_pty.rs`),
//! which macOS does not have. If pty hangup does not reap a producer
//! there, the pipe lease planned for the next milestone stops being
//! belt-and-braces and becomes the only thing between a dead supervisor
//! and an orphaned producer.
//!
//! # Facts 3–6 — the kqueue death watch (`EVFILT_PROC`/`NOTE_EXIT`)
//!
//! Linux proves a peer is gone with a pidfd: it names the *instance*, it is
//! level-triggered (readable forever once the process exits), and an attach
//! to a pid with no process behind it simply fails. macOS has no single
//! object with all three properties. The candidate replacement is a kqueue
//! knote — `kevent(EV_ADD, EVFILT_PROC, NOTE_EXIT, ident = pid)` — which
//! attaches to a `proc`, not to a number, and is therefore the only macOS
//! primitive that can make an *un-fired* registration mean "this pid still
//! names the process I proved". (It has to be: there is no user-space API
//! that reads another process's `pidversion`, so `reverify` cannot be a
//! re-read of the identity the way it is on Linux.) Four kernel behaviours
//! carry that design, and nobody on this project can observe any of them:
//!
//! - **Fact 3 — once, and only once.** The watch treats "an exit was ever
//!   delivered" as proof, and asks the question with a non-blocking drain.
//!   Whether a spent knote re-delivers decides whether the design's exit
//!   latch is load-bearing or redundant.
//! - **Fact 4 — a zombie.** Attach to a process that has exited and has not
//!   been reaped: `ESRCH`, or an attach that fires at once? The design is
//!   safe under either and branches on the answer — but the branch must be
//!   chosen by a test, not by a comment.
//! - **Fact 5 — a reaped pid.** An attach to a pid whose process is fully
//!   gone must FAIL. This is the fail-closed path that makes the challenge's
//!   registration ordering safe.
//! - **Fact 6 — reuse.** The single most load-bearing assumption in the
//!   port: a knote is bound to a process, not to a pid number. Read that
//!   test's own comment for exactly what it proves and what it does not —
//!   forcing a real pid wrap is not something a CI test will do.
//!
//! # A red result here is a decision, not a bug to silence
//!
//! No test here asserts a preference; each pins what the kernel actually
//! does. A failure is an input the owner rules on (accept a weaker macOS
//! identity; promote the pipe lease to load-bearing) — never something to
//! relax until it goes green. Every assertion prints every value it
//! observed, because a CI log is the only channel these facts have.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Bound on every wait in this file. Generous (a cold CI runner spawning a
/// second copy of this test binary is not fast) but finite: nothing here
/// may hang the macOS job.
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(30);
const READ_TIMEOUT: Duration = Duration::from_secs(20);
const CHILD_REAP_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a hung-up pty is given to kill the child before we call it a
/// survivor. Kernel signal delivery is immediate; this is slack, not a
/// measurement.
const PTY_REAP_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the pty child is given to exec and take the pty as its
/// controlling terminal (it announces itself on the tty — see the test).
const PTY_READY_TIMEOUT: Duration = Duration::from_secs(15);

/// Write one observed fact to the process's REAL stderr.
///
/// `println!`/`eprintln!` go through libtest's per-thread output capture,
/// which DISCARDS everything a PASSING test printed — and a passing test
/// is exactly the case where we still want the numbers (which signal
/// killed the child, how long the kernel took). A direct write to the
/// `Stderr` handle does not consult that capture, so these lines reach the
/// CI log either way. Failure messages need no such help: an assertion
/// message is always printed.
fn fact(line: &str) {
    let _ = writeln!(std::io::stderr(), "[macos-kernel-fact] {line}");
}

// ---------------------------------------------------------------------------
// Fact 1: LOCAL_PEERTOKEN, client side
// ---------------------------------------------------------------------------

/// The exact libtest name of the test below — passed to `--exact` when it
/// re-invokes this binary as the client process. Kept as a constant
/// because a mismatch would show up only as "nobody ever connected".
const PEERTOKEN_TEST: &str = "client_fd_reports_the_server_pid_via_local_peertoken";
/// Set (to the socket path) only in that re-invoked child, which makes the
/// test body run its CLIENT half instead of its SERVER half.
const PEERTOKEN_CLIENT_ENV: &str = "SOT_MACOS_FACTS_PEERTOKEN_CLIENT";

/// `audit_token_t` — eight `u32`s. Apple's `audit_token_to_*` accessors
/// define the order: auid, euid, egid, ruid, rgid, pid, asid, pidversion.
/// Declared here rather than taken from `libc`, which exports the
/// `SOL_LOCAL`/`LOCAL_PEERTOKEN` constants for apple targets but not this
/// struct.
#[repr(C)]
#[derive(Clone, Copy)]
struct AuditToken {
    val: [u32; 8],
}

const TOK_AUID: usize = 0;
const TOK_EUID: usize = 1;
const TOK_EGID: usize = 2;
const TOK_RUID: usize = 3;
const TOK_RGID: usize = 4;
const TOK_PID: usize = 5;
const TOK_ASID: usize = 6;
const TOK_PIDVERSION: usize = 7;

/// One observation of `getsockopt(SOL_LOCAL, LOCAL_PEERTOKEN)` on `fd`,
/// including the failure detail — a failed call is itself the answer to
/// fact 1, so `rc`/`errno` are recorded, never unwrapped away.
struct PeerToken {
    rc: i32,
    errno: i32,
    len: u32,
    token: AuditToken,
}

fn read_peer_token(fd: RawFd) -> PeerToken {
    let mut token = AuditToken { val: [0u32; 8] };
    let mut len = std::mem::size_of::<AuditToken>() as libc::socklen_t;
    // SAFETY: `fd` is a live socket owned by the caller for the whole
    // call; the out-buffer is a local `audit_token_t`-shaped struct and
    // `len` is its true size, which the kernel may only shrink.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERTOKEN,
            std::ptr::addr_of_mut!(token).cast(),
            &mut len,
        )
    };
    let errno = if rc == 0 {
        0
    } else {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(-1)
    };
    PeerToken {
        rc,
        errno,
        len: len as u32,
        token,
    }
}

/// `getpeereid` — the uid-only fallback named in the failure messages.
/// Observed (one call) rather than assumed, so a red fact-1 run also says
/// whether the fallback it names is actually there.
fn read_peereid(fd: RawFd) -> (i32, i64, i64) {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: live caller-owned socket fd; both out-params are local.
    let rc = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
    if rc == 0 {
        (0, i64::from(uid), i64::from(gid))
    } else {
        (
            rc,
            i64::from(
                std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(-1),
            ),
            -1,
        )
    }
}

/// Every observed value, as one `k=v` line — the wire format the client
/// reports back over the connection AND the shape both halves print.
fn describe(pt: &PeerToken) -> String {
    format!(
        "rc={} errno={} len={} auid={} euid={} egid={} ruid={} rgid={} pid={} asid={} pidversion={}",
        pt.rc,
        pt.errno,
        pt.len,
        pt.token.val[TOK_AUID],
        pt.token.val[TOK_EUID],
        pt.token.val[TOK_EGID],
        pt.token.val[TOK_RUID],
        pt.token.val[TOK_RGID],
        pt.token.val[TOK_PID],
        pt.token.val[TOK_ASID],
        pt.token.val[TOK_PIDVERSION],
    )
}

/// Pull one `k=v` field out of a report line. The whole raw line is
/// printed in every assertion message, so a missing field degrades to a
/// visibly-wrong sentinel rather than to a panic that hides the report.
fn field(line: &str, key: &str) -> i64 {
    for tok in line.split_whitespace() {
        if let Some((k, v)) = tok.split_once('=') {
            if k == key {
                return v.parse().unwrap_or(i64::MIN);
            }
        }
    }
    i64::MIN
}

/// The CLIENT half, run in the re-invoked child process: connect, read the
/// peer token off our OWN fd (the peer is the server), report back over
/// the very connection we measured.
fn peertoken_client(sock_path: &str) {
    let stream = UnixStream::connect(sock_path)
        .unwrap_or_else(|e| panic!("client: connect to {sock_path} failed: {e}"));
    let fd = stream.as_raw_fd();
    let pt = read_peer_token(fd);
    let (peereid_rc, peereid_uid, peereid_gid) = read_peereid(fd);
    // SAFETY: `getppid` takes no arguments and cannot fail.
    let ppid = unsafe { libc::getppid() };
    let report = format!(
        "{} client_pid={} client_ppid={} peereid_rc={} peereid_uid={} peereid_gid={}",
        describe(&pt),
        std::process::id(),
        ppid,
        peereid_rc,
        peereid_uid,
        peereid_gid,
    );
    fact(&format!(
        "CLIENT view (its own fd; the peer is the SERVER): {report}"
    ));
    let mut stream = stream;
    stream
        .write_all(format!("{report}\n").as_bytes())
        .expect("client: write report");
    stream.flush().expect("client: flush report");
}

/// Reap `child` within `bound`, killing it if it overruns. Returns the
/// status text either way — this is diagnostics, not an assertion.
fn reap_bounded(child: &mut Child, bound: Duration) -> String {
    let deadline = Instant::now() + bound;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return format!("{status}"),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return format!("still running after {bound:?} -- killed");
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return format!("try_wait failed: {e}"),
        }
    }
}

#[test]
fn client_fd_reports_the_server_pid_via_local_peertoken() {
    if let Ok(path) = std::env::var(PEERTOKEN_CLIENT_ENV) {
        peertoken_client(&path);
        return;
    }

    // `tempdir_in("/tmp")`, never `$TMPDIR`: on the macOS runner `$TMPDIR`
    // is ~56 bytes and `sun_path` is 104 there — the same overflow
    // `tests/socket_unix.rs` documents and works around the same way.
    let tmp = tempfile::Builder::new()
        .prefix("sot-mf")
        .tempdir_in("/tmp")
        .expect("tempdir under /tmp");
    let sock = tmp.path().join("pt.sock");
    let listener = UnixListener::bind(&sock)
        .unwrap_or_else(|e| panic!("bind {}: {e}", sock.display()));
    listener
        .set_nonblocking(true)
        .expect("listener set_nonblocking");

    let exe = std::env::current_exe().expect("current_exe");
    let mut child = Command::new(exe)
        .arg("--exact")
        .arg(PEERTOKEN_TEST)
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(PEERTOKEN_CLIENT_ENV, &sock)
        .spawn()
        .expect("spawn the client child");
    let server_pid = std::process::id();
    let client_pid = child.id();

    let deadline = Instant::now() + ACCEPT_TIMEOUT;
    let conn = loop {
        match listener.accept() {
            Ok((s, _)) => break s,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    let status = reap_bounded(&mut child, CHILD_REAP_TIMEOUT);
                    panic!(
                        "no client connected within {ACCEPT_TIMEOUT:?} -- the child \
                         (pid {client_pid}, {status}) never reached `{PEERTOKEN_TEST}`; \
                         socket {}",
                        sock.display()
                    );
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                let status = reap_bounded(&mut child, CHILD_REAP_TIMEOUT);
                panic!("accept failed: {e} (child pid {client_pid}, {status})");
            }
        }
    };
    // BSD accept() can hand back the listener's O_NONBLOCK; make the
    // connection explicitly blocking-with-a-timeout rather than relying on
    // which behaviour this kernel has.
    // Darwin refuses `SO_RCVTIMEO` on an ACCEPTED AF_UNIX socket (EINVAL),
    // though it honours it on a CONNECTED one -- `tests/socket_unix.rs`
    // sets it on the connect side and is green on this leg. So the kernel
    // cannot bound the read below; the deadline in `read_line_bounded`
    // does. Recorded as an observation, never asserted: it is a portability
    // fact about the platform, not the fact this test exists to pin.
    conn.set_nonblocking(true).expect("conn set_nonblocking(true)");
    let rcvtimeo = match conn.set_read_timeout(Some(READ_TIMEOUT)) {
        Ok(()) => "ok".to_string(),
        Err(e) => format!("unsupported ({e})"),
    };

    // The cheap half: what the SERVER sees for its client. Recorded, not
    // asserted — the client's half is the one `authenticate_server` needs.
    let server_view = read_peer_token(conn.as_raw_fd());

    let mut client_report = String::new();
    let read_result = read_line_bounded(&conn, &mut client_report, READ_TIMEOUT);
    let child_status = reap_bounded(&mut child, CHILD_REAP_TIMEOUT);
    let client_report = client_report.trim().to_string();

    let summary = format!(
        "\n  server_pid={server_pid} client_pid={client_pid} child={child_status} so_rcvtimeo={rcvtimeo}\
         \n  CLIENT view (its own fd; the peer is the SERVER): {client_report}\
         \n  SERVER view (its own fd; the peer is the CLIENT): {}",
        describe(&server_view)
    );
    fact(&format!("fact 1 observations:{summary}"));

    match read_result {
        Ok(0) => panic!(
            "the client closed without reporting (EOF) -- it died before or during \
             its measurement.{summary}"
        ),
        Ok(_) => {}
        Err(e) => panic!("reading the client's report failed: {e}.{summary}"),
    }

    // The fallback named below is a DESIGN CHOICE for the owner, not a
    // repair for this test: both halves of it are weaker than what
    // `challenge_unix.rs` gets on Linux.
    const FALLBACK: &str = "The fallback identity is then `getpeereid` (euid/egid only -- \
         no pid at all) plus `proc_pidinfo(PROC_PIDTBSDINFO)`'s `pbi_start_tvsec` as the \
         anti-reuse pin. That is a DECISION FOR THE OWNER, not a bug in this test: do not \
         relax this assertion.";

    let client_rc = field(&client_report, "rc");
    assert_eq!(
        client_rc, 0,
        "LOCAL_PEERTOKEN is NOT available to a CLIENT on this macOS: getsockopt returned \
         rc={client_rc}, errno={}. macOS therefore has no twin of `SO_PEERCRED`'s pid \
         field, and `authenticate_server()` has no strong server identity here. {FALLBACK}{summary}",
        field(&client_report, "errno"),
    );

    let seen_pid = field(&client_report, "pid");
    assert_eq!(
        seen_pid, i64::from(server_pid),
        "LOCAL_PEERTOKEN WORKS but not in the direction `authenticate_server()` needs: the \
         client read its own fd and got pid={seen_pid}, which is not the server's pid \
         ({server_pid}) -- the client's own pid is {client_pid}. If the SERVER view below \
         does carry the client's pid, the mechanism works and only this direction is \
         unavailable. {FALLBACK}{summary}"
    );

    let seen_pidversion = field(&client_report, "pidversion");
    assert!(
        seen_pidversion > 0,
        "LOCAL_PEERTOKEN gave the client the server's pid ({server_pid}) but pidversion=\
         {seen_pidversion}: no reuse generation, so the pid cannot be pinned against reuse \
         the way `challenge_unix.rs` pins it with `/proc/<pid>/stat` start time. The pin \
         would have to come from `proc_pidinfo(PROC_PIDTBSDINFO)`'s `pbi_start_tvsec` \
         instead -- a decision for the owner.{summary}"
    );

    fact(&format!(
        "fact 1 CONFIRMED: a macOS client reading LOCAL_PEERTOKEN on its own fd sees the \
         server's pid={seen_pid} with pidversion={seen_pidversion}"
    ));
}

// ---------------------------------------------------------------------------
// Fact 2: pty hangup
// ---------------------------------------------------------------------------

/// A raw `openpty` + `setsid`/`TIOCSCTTY`/`dup2` spawn, NOT
/// `PtyProducer::spawn`. Deliberate: `PtyProducer` holds the slave for the
/// whole run and its `Drop` kills the child's process group, which is
/// exactly the behaviour under test -- driving it through that type would
/// measure the producer's own killing, not the kernel's. The shape below
/// is `producer_pty.rs`'s own `pre_exec` sequence, minus the Linux-only
/// PDEATHSIG arming that is the reason this fact matters at all.
#[test]
fn closing_the_pty_master_hangs_up_and_reaps_the_child() {
    let mut master_fd: libc::c_int = -1;
    let mut slave_fd: libc::c_int = -1;
    // SAFETY: both out-params are live locals; NULL is documented-valid
    // for `name`/`termp`/`winp` (the pty then takes system defaults).
    let rc = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(
        rc,
        0,
        "openpty failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: both fds were just returned by `openpty` and are owned here.
    let master = unsafe { OwnedFd::from_raw_fd(master_fd) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave_fd) };

    // CLOEXEC on BOTH before the fork, exactly as `producer_pty.rs` does
    // it -- and here it is load-bearing for the measurement, not only for
    // hygiene: if the child inherited a copy of the MASTER, closing the
    // parent's master would not hang the pty up at all and this test would
    // report a false "the child survived". The `dup2`s below re-open the
    // slave on 0..=2 (which clears the flag on those), so the child keeps
    // exactly the slave and nothing else.
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        // SAFETY: both fds are live and owned by this function.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            assert!(flags >= 0, "F_GETFD failed: {}", std::io::Error::last_os_error());
            assert!(
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) >= 0,
                "F_SETFD FD_CLOEXEC failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }

    let slave_raw = slave.as_raw_fd();
    let mut cmd = Command::new("/bin/sh");
    // `printf READY` announces that the child has already run `setsid` +
    // `TIOCSCTTY` (both happen in `pre_exec`, before this shell exists),
    // which closes the race where the master is closed BEFORE the child
    // owns the pty -- a race that would also look like "it survived".
    // `exec sleep 60` then makes the long-lived process the one holding
    // the controlling terminal, with no shell in between.
    cmd.arg("-c").arg("printf READY; exec sleep 60");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: this closure runs between fork and exec -- only
    // async-signal-safe calls, per `pre_exec`'s contract. `setsid`,
    // `ioctl`, `dup2` and `close` all are (same set `producer_pty.rs`'s
    // own `pre_exec` uses).
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(slave_raw, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            for fd in 0..3 {
                if libc::dup2(slave_raw, fd) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            if slave_raw > 2 {
                libc::close(slave_raw);
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().expect("spawn /bin/sh on the pty slave");
    let child_pid = child.id();
    // The parent's own slave copy goes NOW: from here the child's stdio is
    // the only reference to the slave side, so closing the master is a
    // real hangup and nothing else.
    drop(slave);

    // Non-blocking master + a polled read: a bounded wait for READY that
    // cannot wedge the macOS job.
    // SAFETY: `master` is live and owned here.
    unsafe {
        let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
        assert!(flags >= 0, "F_GETFL failed: {}", std::io::Error::last_os_error());
        assert!(
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0,
            "F_SETFL O_NONBLOCK failed: {}",
            std::io::Error::last_os_error()
        );
    }
    let mut master_file = std::fs::File::from(master);
    let mut seen: Vec<u8> = Vec::new();
    let mut ready_err = String::new();
    let ready_deadline = Instant::now() + PTY_READY_TIMEOUT;
    while !seen.windows(5).any(|w| w == b"READY") {
        let mut buf = [0u8; 256];
        match master_file.read(&mut buf) {
            Ok(0) => {
                ready_err = "the pty master reported EOF".into();
                break;
            }
            Ok(n) => seen.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= ready_deadline {
                    ready_err = format!("nothing arrived within {PTY_READY_TIMEOUT:?}");
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => {
                ready_err = format!("reading the pty master failed: {e}");
                break;
            }
        }
    }
    if !ready_err.is_empty() {
        let status = reap_bounded(&mut child, CHILD_REAP_TIMEOUT);
        panic!(
            "the pty child never announced READY ({ready_err}); it never took the pty as \
             its controlling terminal, so this run says nothing about hangup. child pid \
             {child_pid}, {status}, bytes seen on the master: {:?}",
            String::from_utf8_lossy(&seen)
        );
    }

    // THE measurement: close the master, then poll for the child's death.
    let closed_at = Instant::now();
    drop(master_file);
    let deadline = closed_at + PTY_REAP_TIMEOUT;
    let mut outcome = None;
    loop {
        match child.try_wait().expect("try_wait on the pty child") {
            Some(status) => {
                outcome = Some((status, closed_at.elapsed()));
                break;
            }
            None => {
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }

    let Some((status, took)) = outcome else {
        let status = reap_bounded(&mut child, CHILD_REAP_TIMEOUT);
        panic!(
            "the child (pid {child_pid}) SURVIVED closing the pty master for \
             {PTY_REAP_TIMEOUT:?} ({status} after the test killed it). This is a DESIGN \
             INPUT, not a defect in this test: macOS has no `PR_SET_PDEATHSIG`, so if pty \
             hangup does not reap a producer either, the pipe lease becomes the only \
             mechanism that reaps an orphaned producer and must be treated as \
             load-bearing rather than as belt-and-braces. Do not relax this assertion."
        );
    };

    fact(&format!(
        "fact 2: closing the pty master ended the child (pid {child_pid}) in {}ms -- \
         signal={:?} code={:?}",
        took.as_millis(),
        status.signal(),
        status.code()
    ));
    assert!(
        status.signal().is_some(),
        "the child (pid {child_pid}) ended {took:?} after the master closed, but by plain \
         exit (code={:?}), not by a signal -- `sleep 60` cannot exit on its own that fast, \
         so this run did not observe a hangup kill and proves nothing either way. \
         Investigate the spawn, do not relax the assertion.",
        status.code()
    );
}

// ---------------------------------------------------------------------------
// Facts 3-6: the kqueue death watch (EVFILT_PROC / NOTE_EXIT)
// ---------------------------------------------------------------------------

/// How long a `NOTE_EXIT` is given to arrive after the process it names has
/// been killed. Kernel delivery is immediate; this is slack, not a
/// measurement -- the same role `PTY_REAP_TIMEOUT` plays above.
const EXIT_EVENT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a drain that expects to find NOTHING waits before its silence is
/// believed. The non-blocking drain is the one `reverify` will actually do;
/// this second, bounded one is what makes "nothing" mean "the knote is
/// detached" rather than "the kernel had not got to it yet".
const SILENCE_TIMEOUT: Duration = Duration::from_millis(250);
/// How many short-lived children fact 6 spawns to read the pid allocator.
/// The design's own figure. Each is an exec of `/usr/bin/true`, so this is
/// well under a second on a cold runner.
const PID_SAMPLES: usize = 200;
/// How many further processes are created and reaped under a SPENT knote in
/// fact 6, to see whether it re-arms for somebody else.
const REUSE_PROBE_CHILDREN: usize = 24;

/// A fresh `kqueue` fd, closed on drop -- the one-handle-one-kernel-object
/// shape the macOS `PeerProcess` will have (no shared kqueue, no registry).
fn new_kqueue() -> OwnedFd {
    // SAFETY: `kqueue` takes no arguments and returns an owned fd or -1.
    let fd = unsafe { libc::kqueue() };
    assert!(
        fd >= 0,
        "kqueue() failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: `fd` was just returned by `kqueue` and is owned by nobody else.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

/// A zeroed `kevent`, which has no `Default`. Note the struct is
/// `#[repr(packed(4))]` on Apple targets: read its fields BY VALUE into
/// locals, never by reference (`&ev.ident`, or an implicit `{}` borrow in a
/// format string, does not compile).
fn empty_kevent() -> libc::kevent {
    libc::kevent {
        ident: 0,
        filter: 0,
        flags: 0,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

/// The outcome of one `EV_ADD | EV_RECEIPT` of `EVFILT_PROC`/`NOTE_EXIT`.
///
/// `EV_RECEIPT` is the design's own choice (§1 "Registration uses
/// `EV_ADD | EV_RECEIPT`"): it forces the kernel to answer every change with
/// an `EV_ERROR` event carrying the errno in `data`, so an attach failure
/// arrives as an ordinary event instead of as a `kevent` return of -1 the
/// caller would have to disambiguate from "no events were ready". Both
/// spellings are handled here anyway, because which one this kernel uses is
/// itself part of what these tests record.
struct Attach {
    /// `kevent`'s own return: 1 = one receipt, 0 = none, -1 = the call failed.
    rc: i32,
    /// What the kernel said about the ATTACH: 0 = attached, else an errno.
    errno: i32,
    /// Everything observed, for the assertion messages and the CI log.
    detail: String,
}

fn attach_note_exit(kq: RawFd, pid: u32) -> Attach {
    let change = libc::kevent {
        ident: pid as libc::uintptr_t,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_RECEIPT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    let mut out = [empty_kevent()];
    // NEVER a null timeout: on this call it would mean "block forever".
    let ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `kq` is a live kqueue fd owned by the caller; the changelist
    // and eventlist are live locals whose lengths match the counts passed;
    // `ts` is a live local, not null.
    let rc = unsafe { libc::kevent(kq, &change, 1, out.as_mut_ptr(), 1, &ts) };
    if rc < 0 {
        let errno = std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(-1);
        return Attach {
            rc,
            errno,
            detail: format!("kevent(EV_ADD|EV_RECEIPT) returned -1, errno={errno}"),
        };
    }
    if rc == 0 {
        return Attach {
            rc,
            errno: -1,
            detail: "kevent returned 0 receipts for a one-entry EV_RECEIPT changelist \
                     -- EV_RECEIPT is not behaving as documented on this kernel"
                .to_string(),
        };
    }
    // Field reads are by value: `libc::kevent` is packed on Apple targets.
    let (ident, filter, flags, data) = (out[0].ident, out[0].filter, out[0].flags, out[0].data);
    let errno = if flags & libc::EV_ERROR != 0 {
        data as i32
    } else {
        0
    };
    Attach {
        rc,
        errno,
        detail: format!(
            "receipt: ident={ident} filter={filter} flags=0x{flags:x} data={data} (errno={errno})"
        ),
    }
}

/// One drained event, or the absence of one. Every field is recorded rather
/// than unwrapped away: on this path the shape of a non-answer is as much the
/// fact as the answer.
struct Drained {
    rc: i32,
    errno: i32,
    ident: u64,
    filter: i16,
    flags: u16,
    fflags: u32,
    data: i64,
}

fn describe_event(d: &Drained) -> String {
    format!(
        "rc={} errno={} ident={} filter={} flags=0x{:x} fflags=0x{:x} data={}",
        d.rc, d.errno, d.ident, d.filter, d.flags, d.fflags, d.data
    )
}

/// Drain at most one event, waiting at most `bound`.
///
/// `Duration::ZERO` becomes `timespec { 0, 0 }` -- a NON-BLOCKING poll, which
/// is what `reverify` will do. It must never become a null pointer, which
/// `kevent` reads as "block forever": that is the one mistake in this design
/// that type-checks, passes review, and hangs the supervisor's first tick
/// (design §3). The mapping is spelled out here so the tests exercise the
/// same conversion the implementation will.
fn drain_one(kq: RawFd, bound: Duration) -> Drained {
    let ts = libc::timespec {
        tv_sec: i64::try_from(bound.as_secs()).unwrap_or(i64::MAX) as libc::time_t,
        tv_nsec: i64::from(bound.subsec_nanos()) as libc::c_long,
    };
    let mut out = [empty_kevent()];
    // SAFETY: `kq` is a live kqueue fd owned by the caller; no changelist is
    // passed (null + 0 is the documented spelling for "only drain"); the
    // eventlist is a live local of the length passed; `ts` is a live local.
    let rc = unsafe { libc::kevent(kq, std::ptr::null(), 0, out.as_mut_ptr(), 1, &ts) };
    let errno = if rc < 0 {
        std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(-1)
    } else {
        0
    };
    // Field reads are by value: `libc::kevent` is packed on Apple targets.
    let (ident, filter, flags, fflags, data) = (
        out[0].ident,
        out[0].filter,
        out[0].flags,
        out[0].fflags,
        out[0].data,
    );
    Drained {
        rc,
        errno,
        ident: if rc >= 1 { ident as u64 } else { 0 },
        filter: if rc >= 1 { filter } else { 0 },
        flags: if rc >= 1 { flags } else { 0 },
        fflags: if rc >= 1 { fflags } else { 0 },
        data: if rc >= 1 { data as i64 } else { 0 },
    }
}

/// A child that lives until it is killed, with no stdio of its own: these
/// tests want nothing from it but a pid that is alive on demand.
fn spawn_sleeper() -> Child {
    Command::new("/bin/sleep")
        .arg("600")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn /bin/sleep")
}

/// Create one process, let it exit, reap it; return the pid it held.
fn spawn_and_reap_one() -> u32 {
    let mut child = Command::new("/usr/bin/true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn /usr/bin/true");
    let pid = child.id();
    child.wait().expect("reap /usr/bin/true");
    pid
}

/// Fact 3 — is `NOTE_EXIT` delivered once, and what does the NEXT drain say?
///
/// The death watch treats "an exit was ever delivered" as proof the instance
/// is gone, and `reverify` asks that question with a non-blocking drain. Both
/// halves of the contract depend on the answer here and they pull in opposite
/// directions, which is why this is a test and not a comment:
///
/// - if the knote is spent after one delivery, the `AtomicBool` latch in the
///   design is LOAD-BEARING -- it supplies the stickiness a pidfd gets from
///   the kernel for free, and a second `wait` without it would block for the
///   full timeout and then report the process alive: the exact inversion of
///   the truth;
/// - if delivery were level-triggered (pidfd-like) or repeatable, the latch
///   would be redundant and the drain could be a plain re-poll.
///
/// So a red result here is not a bug to silence; it is the design input that
/// decides whether the latch exists.
#[test]
fn note_exit_is_delivered_exactly_once_and_the_knote_is_then_spent() {
    let mut child = spawn_sleeper();
    let pid = child.id();
    let kq = new_kqueue();

    let attach = attach_note_exit(kq.as_raw_fd(), pid);
    fact(&format!(
        "fact 3 attach to a LIVE child (pid {pid}): rc={} errno={} {}",
        attach.rc, attach.errno, attach.detail
    ));
    assert_eq!(
        attach.errno, 0,
        "EV_ADD of EVFILT_PROC/NOTE_EXIT on a LIVE same-user child (pid {pid}) FAILED: {}. \
         The macOS death watch has no other primitive -- `pidversion` is unreadable for \
         another process and `task_for_pid` is entitlement-gated -- so if this is not \
         available the port needs a different mechanism, not a relaxed test.",
        attach.detail
    );

    child.kill().expect("kill the watched child");

    let first = drain_one(kq.as_raw_fd(), EXIT_EVENT_TIMEOUT);
    fact(&format!("fact 3 first drain: {}", describe_event(&first)));
    assert_eq!(
        first.rc, 1,
        "the watched child (pid {pid}) was SIGKILLed and no NOTE_EXIT arrived within \
         {EXIT_EVENT_TIMEOUT:?}: {}. The registration succeeded, so a watch that never \
         fires means `wait` would hang to its deadline and report a dead process alive.",
        describe_event(&first)
    );
    assert_eq!(
        first.flags & libc::EV_ERROR,
        0,
        "the event delivered for pid {pid} is an EV_ERROR, not an exit: {}",
        describe_event(&first)
    );
    assert_eq!(
        first.ident,
        u64::from(pid),
        "the event names ident={} but the watched child is pid {pid}: {}",
        first.ident,
        describe_event(&first)
    );
    assert_eq!(
        first.filter,
        libc::EVFILT_PROC,
        "the event came from filter={}, not EVFILT_PROC ({}): {}",
        first.filter,
        libc::EVFILT_PROC,
        describe_event(&first)
    );
    assert!(
        first.fflags & libc::NOTE_EXIT != 0,
        "the event for pid {pid} carries no NOTE_EXIT bit: {}",
        describe_event(&first)
    );

    // THE question the latch exists for. Non-blocking, exactly as `reverify`
    // will ask it.
    let second = drain_one(kq.as_raw_fd(), Duration::ZERO);
    fact(&format!(
        "fact 3 second drain (non-blocking): {}",
        describe_event(&second)
    ));
    assert_eq!(
        second.rc, 0,
        "a SECOND drain of the same kqueue returned an event ({}). NOTE_EXIT is therefore \
         NOT once-only on this kernel -- it is repeatable or level-triggered. That is a \
         DESIGN INPUT, not a defect in this test: it would make the `AtomicBool` latch \
         redundant and change what `reverify`'s drain means. Rule on it; do not relax \
         this assertion.",
        describe_event(&second)
    );

    // Reaping is the other thing that could plausibly re-deliver: the design
    // calls `reap` (waitpid WNOHANG) only after this event, so pin that the
    // reap itself puts nothing back on the queue.
    let status = child.wait().expect("reap the watched child");
    let third = drain_one(kq.as_raw_fd(), SILENCE_TIMEOUT);
    fact(&format!(
        "fact 3 third drain, after waitpid ({status}): {}",
        describe_event(&third)
    ));
    assert_eq!(
        third.rc, 0,
        "reaping the child (pid {pid}, {status}) delivered a further event ({}) on a \
         knote whose NOTE_EXIT had already been consumed.",
        describe_event(&third)
    );

    fact(&format!(
        "fact 3 CONFIRMED: NOTE_EXIT for pid {pid} was delivered exactly once (data={}, \
         i.e. the wait status) and the knote is then spent -- the latch is load-bearing",
        first.data
    ));
}

/// Fact 4 — what happens when you attach to a ZOMBIE?
///
/// The challenge registers the watch between the same-user check and the
/// identity exchange, so it can meet a peer that has already exited but has
/// not been reaped. The design is safe under either answer and branches on
/// it -- `ESRCH` means "not attachable", and who owns the pid decides what
/// that means (our own child: provably exited, since the zombie pins the pid;
/// a peer we did not spawn: `Undetermined`). The branch must be chosen by a
/// test rather than by a comment, which is this test.
///
/// There is exactly one answer the design CANNOT absorb, and it is the one
/// this test is really hunting: an attach that SUCCEEDS and then never fires.
/// That would be a watch that reports a dead process alive forever.
#[test]
fn attaching_to_an_unreaped_zombie_either_fails_esrch_or_fires_at_once() {
    let mut child = spawn_sleeper();
    let pid = child.id();

    // The witness watch, registered while the child is still alive, is how
    // this test knows the child has EXITED without calling `wait` on it --
    // `waitpid` would reap it and destroy the very state under measurement.
    let witness = new_kqueue();
    let witness_attach = attach_note_exit(witness.as_raw_fd(), pid);
    assert_eq!(
        witness_attach.errno, 0,
        "the witness attach to a live child (pid {pid}) failed: {}",
        witness_attach.detail
    );
    child.kill().expect("kill the child that becomes the zombie");
    let seen = drain_one(witness.as_raw_fd(), EXIT_EVENT_TIMEOUT);
    assert_eq!(
        seen.rc, 1,
        "the witness watch never reported the exit of pid {pid} ({}), so this run never \
         reached the state it exists to measure -- an exited, UNREAPED process. Fact 3 \
         says whether the watch itself is the problem.",
        describe_event(&seen)
    );
    // `child` is deliberately NOT waited on here: the process is a zombie,
    // and while we hold it the kernel cannot recycle its pid, so `pid` below
    // unambiguously names it.

    let kq = new_kqueue();
    let attach = attach_note_exit(kq.as_raw_fd(), pid);
    fact(&format!(
        "fact 4 attach to an UNREAPED ZOMBIE (pid {pid}): rc={} errno={} {}",
        attach.rc, attach.errno, attach.detail
    ));

    if attach.errno != 0 {
        assert_eq!(
            attach.errno,
            libc::ESRCH,
            "attaching to an unreaped zombie (pid {pid}) failed with errno={}, which is \
             neither success nor ESRCH ({}): {}. The design reads ESRCH as \"not \
             attachable\" and gives it an owner-dependent meaning; a third errno has no \
             reading at all and must be ruled on.",
            attach.errno,
            libc::ESRCH,
            attach.detail
        );
        fact(&format!(
            "fact 4 BRANCH A: EV_ADD on an unreaped zombie fails ESRCH -- `proc_find` does \
             not return zombies. probe_macos::SpawnedChild must read ESRCH as 'already \
             exited' (the zombie pins the pid); ChallengedProcess must read it as \
             Undetermined."
        ));
    } else {
        let ev = drain_one(kq.as_raw_fd(), EXIT_EVENT_TIMEOUT);
        fact(&format!(
            "fact 4 BRANCH B drain after attaching to a zombie: {}",
            describe_event(&ev)
        ));
        assert_eq!(
            ev.rc, 1,
            "EV_ADD on an unreaped zombie (pid {pid}) SUCCEEDED and then delivered NOTHING \
             within {EXIT_EVENT_TIMEOUT:?}: {}. This is the ONE outcome the death watch \
             cannot absorb: the registration reports success, so nothing fails closed, and \
             the exit it was meant to report has already happened and will never happen \
             again -- every caller reads 'still alive' forever. The registration ordering \
             in the challenge would have to change. Do not relax this assertion.",
            describe_event(&ev)
        );
        assert_eq!(
            ev.ident,
            u64::from(pid),
            "the immediate event names ident={} but the zombie is pid {pid}: {}",
            ev.ident,
            describe_event(&ev)
        );
        assert!(
            ev.fflags & libc::NOTE_EXIT != 0,
            "the immediate event for zombie pid {pid} carries no NOTE_EXIT bit: {}",
            describe_event(&ev)
        );
        fact(
            "fact 4 BRANCH B: EV_ADD on an unreaped zombie SUCCEEDS and fires NOTE_EXIT at \
             once. Both owners can then read it the same way -- attach, drain, and an \
             immediate event means 'already exited'.",
        );
    }

    let status = child.wait().expect("reap the zombie");
    fact(&format!("fact 4: the zombie (pid {pid}) reaped: {status}"));
}

/// Fact 5 — attaching to a pid whose process is fully gone must FAIL.
///
/// This is the fail-closed path the registration ordering rests on. The
/// challenge attaches BEFORE it writes its request, so that a peer which died
/// in the window cannot yield a silently-dead watch: the attach itself
/// refuses, and the challenge returns `Undetermined`. If `EV_ADD` instead
/// succeeded on a pid with no process behind it, that refusal would not
/// exist and the ordering argument would collapse.
#[test]
fn attaching_to_a_reaped_pid_fails_with_esrch() {
    let mut child = spawn_sleeper();
    let pid = child.id();
    child.kill().expect("kill the child before reaping it");
    let status = child.wait().expect("reap the child");
    // From here `pid` names nothing: the process is gone and its pid is back
    // in the allocator's pool. Darwin allocates pids sequentially and wraps
    // at PID_MAX (fact 6), so handing this exact number to a new process
    // between the two statements below would take a full wrap -- roughly 1e5
    // intervening process creations. If this test ever goes red having
    // ATTACHED, check that first, but read it as the fact having changed.
    let kq = new_kqueue();
    let attach = attach_note_exit(kq.as_raw_fd(), pid);
    fact(&format!(
        "fact 5 attach to a REAPED pid ({pid}, {status}): rc={} errno={} {}",
        attach.rc, attach.errno, attach.detail
    ));

    assert_ne!(
        attach.errno, 0,
        "EV_ADD of EVFILT_PROC/NOTE_EXIT on a fully REAPED pid ({pid}, {status}) SUCCEEDED: \
         {}. There is then no fail-closed path at all: a watch registered on a peer that \
         died in the challenge window would attach to nothing, report no exit, and be read \
         as 'the peer is alive' forever. The whole registration ordering depends on this \
         refusal.",
        attach.detail
    );
    assert_eq!(
        attach.errno,
        libc::ESRCH,
        "EV_ADD on a fully REAPED pid ({pid}) failed with errno={}, not ESRCH ({}): {}. \
         The implementation maps ESRCH to a specific, owner-dependent meaning (fact 4); \
         another errno has no mapping and must be ruled on rather than lumped in.",
        attach.errno,
        libc::ESRCH,
        attach.detail
    );

    let ev = drain_one(kq.as_raw_fd(), SILENCE_TIMEOUT);
    assert_eq!(
        ev.rc, 0,
        "the attach to reaped pid {pid} failed with ESRCH and yet the kqueue delivered an \
         event ({}) -- a failed registration must leave no knote behind.",
        describe_event(&ev)
    );

    fact(&format!(
        "fact 5 CONFIRMED: EV_ADD on a reaped pid ({pid}) fails ESRCH and leaves no knote"
    ));
}

/// Fact 6 — the load-bearing assumption of the whole macOS port, and the
/// limits of what a test on a CI runner can say about it.
///
/// The claim the port rests on: a knote is bound to a *process*, not to a pid
/// *number*, so a registration that has NOT fired is a proof of identity --
/// which is what lets `terminate` send `kill(pid, SIGKILL)` by number and
/// what lets `reverify` answer from the queue alone.
///
/// WHAT THIS TEST PROVES:
///
/// 1. Darwin allocates pids sequentially (strictly increasing, at most one
///    wraparound across {PID_SAMPLES} consecutive children). This is the
///    bound the design quotes and has so far only asserted in prose: reuse of
///    one specific number requires a full wrap, on the order of 1e5
///    intervening process creations. A fork storm makes those creations take
///    LONGER, so the bound does not shrink under load.
/// 2. A knote fires for ITS target and not for another process: a watch on a
///    second, still-living child stays silent while the first one dies.
/// 3. A SPENT knote never re-arms. After its NOTE_EXIT is consumed and its
///    target reaped, it stays silent across the creation, exit and reaping of
///    {REUSE_PROBE_CHILDREN} further processes.
///
/// WHAT THIS TEST DOES *NOT* PROVE: that a knote fails to fire for a process
/// which actually receives the watched pid number. Forcing that requires
/// wrapping PID_MAX -- ~1e5 process creations on a shared CI runner, whose
/// own processes are also drawing from the same allocator, so the landing is
/// not even deterministic. This test deliberately does not attempt it, and
/// asserts instead that none of the probe children DID receive the watched
/// pid -- which is precisely the reason it cannot observe a reuse, made
/// explicit rather than left as an unstated gap.
///
/// The instance-binding claim therefore rests on three pinned facts and one
/// unobserved step: the kernel resolves the ident to a process AT ATTACH TIME
/// and refuses when there is none (fact 5), delivery is once-only and the
/// knote is then spent (fact 3), a spent knote never re-arms (3 above) -- and
/// the unobserved step is that the kernel does not re-resolve a stored ident
/// against a later process. Nothing short of a real wrap observes that step;
/// if a future reader finds a way to force one cheaply, this is the test to
/// extend.
#[test]
fn pid_reuse_needs_a_full_sequential_wrap_and_a_spent_knote_never_rearms() {
    // --- 1. the allocator ---------------------------------------------------
    let mut pids = Vec::with_capacity(PID_SAMPLES);
    for _ in 0..PID_SAMPLES {
        pids.push(spawn_and_reap_one());
    }
    let descents: Vec<(u32, u32)> = pids
        .windows(2)
        .filter(|w| w[1] <= w[0])
        .map(|w| (w[0], w[1]))
        .collect();
    fact(&format!(
        "fact 6 allocator: {PID_SAMPLES} children, first={} last={} descents={:?}",
        pids[0],
        pids[PID_SAMPLES - 1],
        descents
    ));
    assert!(
        descents.len() <= 1,
        "Darwin no longer allocates pids sequentially: across {PID_SAMPLES} consecutive \
         children the pid went down or stood still {} times ({descents:?}). At most one \
         such step is a wraparound at PID_MAX; more than one means the allocator has been \
         randomised. That would delete the only bound the port has on pid reuse -- \
         `terminate`'s numeric `kill(pid, SIGKILL)` and the challenge's attach window are \
         both argued from \"reuse of a specific number takes a full wrap\". Do not relax \
         this assertion; it is the premise that would have changed.",
        descents.len()
    );

    // --- 2. a knote fires for its own target only ---------------------------
    let mut first = spawn_sleeper();
    let mut second = spawn_sleeper();
    let first_pid = first.id();
    let second_pid = second.id();
    let kq_first = new_kqueue();
    let kq_second = new_kqueue();
    for (kq, pid) in [(&kq_first, first_pid), (&kq_second, second_pid)] {
        let attach = attach_note_exit(kq.as_raw_fd(), pid);
        assert_eq!(
            attach.errno, 0,
            "attach to live child pid {pid} failed: {}",
            attach.detail
        );
    }

    first.kill().expect("kill the first child");
    let ev = drain_one(kq_first.as_raw_fd(), EXIT_EVENT_TIMEOUT);
    assert_eq!(
        ev.rc, 1,
        "the watch on pid {first_pid} did not report its exit ({}); fact 3 covers the \
         delivery itself, so this run cannot speak to what follows.",
        describe_event(&ev)
    );
    assert_eq!(
        ev.ident,
        u64::from(first_pid),
        "the watch on pid {first_pid} reported ident={} instead: {}",
        ev.ident,
        describe_event(&ev)
    );
    let bystander = drain_one(kq_second.as_raw_fd(), SILENCE_TIMEOUT);
    assert_eq!(
        bystander.rc, 0,
        "the watch registered on pid {second_pid} (still alive) fired when a DIFFERENT \
         process (pid {first_pid}) exited: {}. A knote that reports other processes' \
         exits names nothing, and an un-fired registration would stop being a proof of \
         anything.",
        describe_event(&bystander)
    );
    let first_status = first.wait().expect("reap the first child");

    // --- 3. a spent knote never re-arms -------------------------------------
    let mut probe_pids = Vec::with_capacity(REUSE_PROBE_CHILDREN);
    for _ in 0..REUSE_PROBE_CHILDREN {
        probe_pids.push(spawn_and_reap_one());
    }
    assert!(
        !probe_pids.contains(&first_pid),
        "one of the {REUSE_PROBE_CHILDREN} probe children was handed pid {first_pid} -- the \
         very number this test's spent knote watches. That is the reuse this test says it \
         cannot force, so if it happens the run is no longer the approximation documented \
         above: record what the spent knote did ({:?}) and rewrite this test around the \
         real observation.",
        probe_pids
    );
    let spent = drain_one(kq_first.as_raw_fd(), SILENCE_TIMEOUT);
    assert_eq!(
        spent.rc, 0,
        "the SPENT knote for pid {first_pid} ({first_status}) delivered a further event \
         ({}) after {REUSE_PROBE_CHILDREN} unrelated processes were created and reaped. \
         It has re-armed for somebody, which is the failure mode the whole macOS identity \
         argument excludes.",
        describe_event(&spent)
    );

    // The second watch is still live and still silent: its target never died.
    let still_alive = drain_one(kq_second.as_raw_fd(), Duration::ZERO);
    assert_eq!(
        still_alive.rc, 0,
        "the watch on pid {second_pid} fired although that child is still running: {}",
        describe_event(&still_alive)
    );
    second.kill().expect("kill the second child");
    let ev_second = drain_one(kq_second.as_raw_fd(), EXIT_EVENT_TIMEOUT);
    assert_eq!(
        ev_second.rc, 1,
        "the watch on pid {second_pid} never reported its own exit ({}) -- having stayed \
         silent for the right reason, it must still speak for the right one.",
        describe_event(&ev_second)
    );
    assert_eq!(
        ev_second.ident,
        u64::from(second_pid),
        "the watch on pid {second_pid} reported ident={} at its own exit: {}",
        ev_second.ident,
        describe_event(&ev_second)
    );
    let second_status = second.wait().expect("reap the second child");

    fact(&format!(
        "fact 6 CONFIRMED (to the limit stated on the test): pids sequential over \
         {PID_SAMPLES} children with {} wrap(s); the watch on {first_pid} ({first_status}) \
         fired once for its own target and stayed spent across {REUSE_PROBE_CHILDREN} \
         later processes; the watch on {second_pid} ({second_status}) stayed silent \
         throughout and then reported its own exit",
        descents.len()
    ));
}

/// Read one newline-terminated report with OUR deadline rather than the
/// kernel's: Darwin refuses `SO_RCVTIMEO` on an accepted AF_UNIX socket, so
/// the socket is non-blocking and the bound lives here. Returns what
/// `read_line` would: the byte count, `Ok(0)` for a clean EOF with nothing
/// said, and `TimedOut` when the peer went quiet -- each of which the
/// caller already renders with the full observation block.
fn read_line_bounded(
    conn: &UnixStream,
    out: &mut String,
    bound: Duration,
) -> std::io::Result<usize> {
    let deadline = Instant::now() + bound;
    let mut reader = conn;
    let mut buf = [0u8; 512];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return Ok(out.len()),
            Ok(n) => {
                out.push_str(&String::from_utf8_lossy(&buf[..n]));
                if out.contains('\n') {
                    return Ok(out.len());
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        ErrorKind::TimedOut,
                        format!("the client said nothing within {bound:?}"),
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e),
        }
    }
}
