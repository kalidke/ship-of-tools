//! SocketClient tests: cancel from another thread, concurrent submit, latched failures, connect fast-fail and backlog
//! retry, an excess connection at capacity.

use super::*;
use sot_platform::transport::connect_own::connect_own;
use sot_log::row::RowClient;

/// A `SocketClient::read` blocked on one thread is unblocked by `cancel()` called from another, returning
/// `TransportError::Cancelled`.
#[test]
fn client_read_cancel_unblocks_from_another_thread() {
    if !run_isolated("client::client_read_cancel_unblocks_from_another_thread") {
        return;
    }
    let (_dir, path, server) = bind_row();
    let client = raw_client(&path);

    let reader_client = Arc::clone(&client);
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        reader_client.read(&mut buf) // blocks: the row answers nothing before a hello
    });
    std::thread::sleep(Duration::from_millis(300)); // let the read actually become pending
    client.cancel();

    let result = sot_platform::test_wait::within(TIMEOUT, "the cancelled reader", move || reader.join()).unwrap();
    assert!(matches!(result, Err(TransportError::Cancelled)), "expected Cancelled, got {result:?}");
    server.shutdown();
}

/// A `SocketClient::write_all` blocked on one thread (the socket's buffer saturated because the peer never reads) is
/// unblocked by `cancel()` called from another.
#[test]
fn client_write_cancel_unblocks_from_another_thread() {
    if !run_isolated("client::client_write_cancel_unblocks_from_another_thread") {
        return;
    }
    let dir = private_tmp();
    let path = row_path(dir.path());
    // A peer that accepts and never reads: the row would end a connection that sends it a flood, so a plain listener.
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    let client = raw_client(&path);
    let (_held, _) = listener.accept().expect("accept");

    let writer_client = Arc::clone(&client);
    let writer = std::thread::spawn(move || {
        let payload = vec![0xCDu8; 65_536];
        loop {
            if let Err(e) = writer_client.write_all(&payload) {
                return e;
            }
        }
    });
    std::thread::sleep(Duration::from_secs(2)); // let the flood saturate the socket buffer
    client.cancel();

    let result = sot_platform::test_wait::within(TIMEOUT, "the cancelled writer", move || writer.join()).unwrap();
    assert!(matches!(result, TransportError::Cancelled), "expected Cancelled, got {result:?}");
}

/// A SECOND concurrent same-direction `read` returns `TransportError::ConcurrentSubmit` rather than racing the first.
#[test]
fn concurrent_same_direction_client_read_returns_distinct_error() {
    if !run_isolated("client::concurrent_same_direction_client_read_returns_distinct_error") {
        return;
    }
    let (_dir, path, server) = bind_row();
    let client = raw_client(&path);

    let a = Arc::clone(&client);
    let reader_a = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        a.read(&mut buf) // blocks: nobody ever sends
    });
    let deadline = Instant::now() + TIMEOUT;
    while !client.read_slot_entered_for_test() {
        assert!(Instant::now() < deadline, "timed out waiting for A's read to genuinely enter the read slot");
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut buf_b = [0u8; 16];
    let result_b = client.read(&mut buf_b);
    assert!(matches!(result_b, Err(TransportError::ConcurrentSubmit)), "expected ConcurrentSubmit, got {result_b:?}");

    client.cancel();
    let result_a = sot_platform::test_wait::within(TIMEOUT, "the cancelled first reader", move || reader_a.join()).unwrap();
    assert!(matches!(result_a, Err(TransportError::Cancelled)), "expected Cancelled, got {result_a:?}");
    server.shutdown();
}

#[test]
fn cancelled_client_rejects_later_submissions() {
    if !run_isolated("client::cancelled_client_rejects_later_submissions") {
        return;
    }
    let (_dir, path, server) = bind_row();
    let client = raw_client(&path);
    client.cancel();

    assert!(matches!(client.write_all(b"x"), Err(TransportError::Cancelled)), "a cancelled client must reject a later write");
    let mut buf = [0u8; 16];
    assert!(matches!(client.read(&mut buf), Err(TransportError::Cancelled)), "a cancelled client must reject a later read");
    server.shutdown();
}

/// A write that fails terminally surfaces as its own `Io` error, never `Cancelled`, and latches the connection closed:
/// every later call is `Cancelled`.
#[test]
fn a_terminal_write_failure_latches_the_connection_closed() {
    if !run_isolated("client::a_terminal_write_failure_latches_the_connection_closed") {
        return;
    }
    let (_dir, path, server) = bind_row();
    let client = raw_client(&path);

    // A first line that is no frame: the row ends the connection.
    client.write_all(b"this is no frame\n").unwrap();
    let c = Arc::clone(&client);
    let eof = sot_platform::test_wait::within(TIMEOUT, "the row's close", move || c.read(&mut [0u8; 16]));
    assert!(matches!(eof, Ok(0)), "the row ends the connection: {eof:?}");

    let chunk = vec![0xABu8; 64 * 1024];
    let mut result = Ok(());
    for _ in 0..256 {
        result = client.write_all(&chunk);
        if result.is_err() {
            break;
        }
    }
    assert!(
        matches!(result, Err(TransportError::Io { .. })),
        "expected the terminal write failure to surface as its OWN Io error (never Cancelled: nobody called cancel()), got {result:?}"
    );
    let c = Arc::clone(&client);
    let read = sot_platform::test_wait::within(TIMEOUT, "the latched client's read", move || c.read(&mut [0u8; 16]));
    assert!(matches!(read, Err(TransportError::Cancelled)), "a client whose write already failed terminally must reject a later read: {read:?}");
    assert!(
        matches!(client.write_all(b"x"), Err(TransportError::Cancelled)),
        "a client whose write already failed terminally must reject a later write"
    );
    server.shutdown();
}

#[test]
#[cfg(target_os = "linux")]
fn connect_fails_fast_when_nothing_listens() {
    if !run_isolated("client::connect_fails_fast_when_nothing_listens") {
        return;
    }
    let dir = private_tmp();
    let path = row_path(dir.path());

    let started = Instant::now();
    let err = connect_own(&path).unwrap_err();
    let elapsed = started.elapsed();

    eprintln!("connect_fails_fast_when_nothing_listens: elapsed={elapsed:?}");
    assert!(matches!(err, TransportError::Io { op, .. } if op.contains("connect")), "expected a connect-family error, got {err}");
    assert!(
        elapsed < CONNECT_BOUND,
        "an absent endpoint (ENOENT) must fail on the FIRST attempt, never consume the full {CONNECT_BOUND:?}: took {elapsed:?}"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn connect_fails_fast_when_refused_by_a_stale_socket_file() {
    if !run_isolated("client::connect_fails_fast_when_refused_by_a_stale_socket_file") {
        return;
    }
    let dir = private_tmp();
    let path = row_path(dir.path());
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());

    let started = Instant::now();
    let err = connect_own(&path).unwrap_err();
    let elapsed = started.elapsed();

    eprintln!("connect_fails_fast_when_refused_by_a_stale_socket_file: elapsed={elapsed:?}");
    assert!(matches!(err, TransportError::Io { op, .. } if op.contains("connect")), "expected a connect-family error, got {err}");
    assert!(
        elapsed < CONNECT_BOUND,
        "ECONNREFUSED (no listener) must fail on the FIRST attempt, never consume the full {CONNECT_BOUND:?}: took {elapsed:?}"
    );
}

/// One raw non-blocking `connect(2)` to `path`: the stream, or `None` on `EAGAIN` (the backlog is full).
#[cfg(target_os = "linux")]
fn nonblocking_connect_attempt(path: &Path) -> std::io::Result<Option<UnixStream>> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::FromRawFd;
    unsafe {
        let raw = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_NONBLOCK, 0);
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let path_bytes = path.as_os_str().as_bytes();
        assert!(path_bytes.len() < addr.sun_path.len(), "test socket path too long for sockaddr_un");
        for (dst, &b) in addr.sun_path.iter_mut().zip(path_bytes) {
            *dst = b as libc::c_char;
        }
        let addr_len = (std::mem::size_of::<libc::sa_family_t>() + path_bytes.len() + 1) as libc::socklen_t;
        if libc::connect(raw, std::ptr::addr_of!(addr).cast(), addr_len) == 0 {
            return Ok(Some(UnixStream::from_raw_fd(raw)));
        }
        let err = std::io::Error::last_os_error();
        libc::close(raw);
        if err.raw_os_error() == Some(libc::EAGAIN) {
            Ok(None)
        } else {
            Err(err)
        }
    }
}

#[test]
#[cfg(target_os = "linux")]
fn connect_retries_within_the_bound_on_a_full_backlog() {
    if !run_isolated("client::connect_retries_within_the_bound_on_a_full_backlog") {
        return;
    }
    let dir = private_tmp();
    let path = row_path(dir.path());
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    use std::os::unix::io::AsRawFd as _;
    const BACKLOG: i32 = 4;
    assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), BACKLOG) }, 0, "listen(2) must reset the backlog");

    let mut saturating = Vec::new();
    let mut observed_eagain = false;
    for _ in 0..64 {
        match nonblocking_connect_attempt(&path).expect("raw connect(2) setup failed") {
            Some(s) => saturating.push(s),
            None => {
                observed_eagain = true;
                break;
            }
        }
    }
    assert!(observed_eagain, "expected the backlog to fill (a real EAGAIN) within 64 raw connects");

    let (client, elapsed) = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(200));
            let _ = listener.accept();
        });
        let started = Instant::now();
        let client = connect_own(&path).expect("expected the retry loop to succeed once a backlog slot freed");
        (client, started.elapsed())
    });

    eprintln!("connect_retries_within_the_bound_on_a_full_backlog: elapsed={elapsed:?}");
    assert!(elapsed < CONNECT_BOUND, "expected the retry to succeed comfortably inside {CONNECT_BOUND:?}, took {elapsed:?}");
    drop((client, listener, saturating));
}

/// The row holds eight connections; a ninth is accepted by the kernel and then ended by the row at once, which a
/// client sees as an early, ordered EOF rather than a hang.
#[test]
fn excess_connection_at_capacity_is_seen_as_early_eof() {
    if !run_isolated("client::excess_connection_at_capacity_is_seen_as_early_eof") {
        return;
    }
    // The challenge refuses a peer that started in the tick it connects in; this binary is both ends.
    std::thread::sleep(Duration::from_millis(50));
    let (_dir, path, server) = bind_row();
    let held: Vec<RowClient> = (0..8).map(|_| RowClient::connect(&path, "t", TIMEOUT).expect("one of eight")).collect();

    let ninth = raw_client(&path);
    let n = sot_platform::test_wait::within(TIMEOUT, "the excess connection's read", move || ninth.read(&mut [0u8; 16]))
        .expect("read should observe an ordered EOF, not an error");
    assert_eq!(n, 0, "expected the excess connection to see EOF promptly");
    drop(held);
    server.shutdown();
}
