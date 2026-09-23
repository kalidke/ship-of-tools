#![cfg(target_os = "macos")]
//! macOS kernel-fact regression tests — the two OS behaviours the macOS
//! lane is blocked on, pinned here so that a future OS change reports
//! itself as a named red test rather than as a silent auth or capsule
//! regression months later.
//!
//! The whole file is `#![cfg(target_os = "macos")]`: it compiles away to
//! nothing everywhere else, so the blocking `macos-latest` CI job
//! (`cargo test --workspace --locked`) is the only place either test runs.
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
//! # A red result here is a decision, not a bug to silence
//!
//! Neither test asserts a preference; each pins what the kernel actually
//! does. A failure is an input the owner rules on (accept a weaker macOS
//! identity; promote the pipe lease to load-bearing) — never something to
//! relax until it goes green. Every assertion prints every value it
//! observed, because a CI log is the only channel these facts have.

use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
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
