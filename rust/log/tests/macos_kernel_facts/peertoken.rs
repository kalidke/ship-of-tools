//! Fact 1: LOCAL_PEERTOKEN read on the client's own fd reports the server's pid.

use super::*;

// ---------------------------------------------------------------------------
// Fact 1: LOCAL_PEERTOKEN, client side
// ---------------------------------------------------------------------------

/// The exact libtest name of the test below — passed to `--exact` when it
/// re-invokes this binary as the client process. Kept as a constant
/// because a mismatch would show up only as "nobody ever connected".
const PEERTOKEN_TEST: &str = "peertoken::client_fd_reports_the_server_pid_via_local_peertoken";
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
    // Hold the connection until the server has read ITS view of us (it writes one byte after its read): a
    // peer that has already exited has no token to read (`rc=-1 errno=57`), which proves nothing either way.
    let mut release = [0u8; 1];
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.read(&mut release);
}

#[test]
#[allow(clippy::too_many_lines, reason = "one test scenario: the client fd reports the server pid through the peer token")]
fn client_fd_reports_the_server_pid_via_local_peertoken() {
    if let Ok(path) = std::env::var(PEERTOKEN_CLIENT_ENV) {
        peertoken_client(&path);
        return;
    }

    // `tempdir_in("/tmp")`, never `$TMPDIR`: on the macOS runner `$TMPDIR`
    // is ~56 bytes and `sun_path` is 104 there — the same overflow
    // `tests/socket_unix/` documents and works around the same way.
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
    // though it honours it on a CONNECTED one -- `tests/socket_unix/`
    // sets it on the connect side and is green on this leg. So the kernel
    // cannot bound the read below; the deadline in `read_line_bounded`
    // does. Recorded as an observation, never asserted: it is a portability
    // fact about the platform, not the fact this test exists to pin.
    conn.set_nonblocking(true).expect("conn set_nonblocking(true)");
    let rcvtimeo = match conn.set_read_timeout(Some(READ_TIMEOUT)) {
        Ok(()) => "ok".to_string(),
        Err(e) => format!("unsupported ({e})"),
    };

    let mut client_report = String::new();
    let read_result = read_line_bounded(&conn, &mut client_report, READ_TIMEOUT);
    // The client is still connected, waiting for the byte below, so the server's read of its view of the client
    // is made while the peer exists. Taken before the release, so no path out of this function skips it.
    let server_view = read_peer_token(conn.as_raw_fd());
    let server_peereid = read_peereid(conn.as_raw_fd());
    let _ = (&conn).write_all(b"x");
    let child_status = reap_bounded(&mut child, CHILD_REAP_TIMEOUT);
    let client_report = client_report.trim().to_string();

    let summary = format!(
        "\n  server_pid={server_pid} client_pid={client_pid} child={child_status} so_rcvtimeo={rcvtimeo}\
         \n  CLIENT view (its own fd; the peer is the SERVER): {client_report}\
         \n  SERVER view (its own fd; the peer is the CLIENT): {}\
         \n  SERVER getpeereid (rc, uid, gid): {server_peereid:?}",
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

    // The server's half, the one that admits a connection at accept (`server/listen.rs` `admit_peer`): the token
    // is the client's whole audit token, so the client's pid in it proves the euid word is the client's too.
    // SAFETY: geteuid has no preconditions and cannot fail.
    let own_euid = unsafe { libc::geteuid() };
    assert_eq!(
        (server_view.rc, server_view.errno), (0, 0),
        "LOCAL_PEERTOKEN is NOT available to a SERVER on its accepted fd, with the client still connected: \
         getsockopt returned rc={}, errno={}.{summary}",
        server_view.rc, server_view.errno,
    );
    assert_eq!(
        server_view.token.val[TOK_PID], client_pid,
        "the server read its accepted fd and got another pid than its client's ({client_pid}).{summary}"
    );
    assert!(
        server_view.token.val[TOK_PIDVERSION] > 0,
        "the server's view of its client carries no pidversion.{summary}"
    );
    assert_eq!(
        server_view.token.val[TOK_EUID], own_euid,
        "the client's euid in the server's token is not ours ({own_euid}); the client is this process's child.{summary}"
    );

    fact(&format!(
        "fact 1 CONFIRMED: a macOS client reading LOCAL_PEERTOKEN on its own fd sees the \
         server's pid={seen_pid} with pidversion={seen_pidversion}; the server reading its accepted fd sees \
         the client's pid={client_pid} with pidversion={} and euid={own_euid}",
        server_view.token.val[TOK_PIDVERSION]
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
