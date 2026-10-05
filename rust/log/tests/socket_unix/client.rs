//! SocketClient tests: cancel from another thread, concurrent submit, latched failures, connect fast-fail and backlog retry, capacity EOF.

use super::*;

// ---------------------------------------------------------------------
// L1-unix LU1c: `SocketClient` (`write_all`/`read`/`cancel`, and the
// bounded connect retry loop). Mirrors the analogous section of
// `tests/pipe_win/`. `SocketClient::from_stream_for_test` builds a
// client around a plain `UnixStream::connect` — the `pub(crate)`
// unchallenged constructors are not reachable from this separate
// integration-test crate, exactly like `pipe_win::
// connect_voyage_pipe_unchallenged` is not reachable from
// `tests/pipe_win/` either.
// ---------------------------------------------------------------------

/// A `SocketClient::read` blocked on one thread is unblocked by
/// `cancel()` called from another, returning `TransportError::Cancelled`.
#[test]
fn client_read_cancel_unblocks_from_another_thread() {
    if !run_isolated("client::client_read_cancel_unblocks_from_another_thread") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();
    let client = Arc::new(SocketClient::from_stream_for_test(
        UnixStream::connect(&path).unwrap(),
        0,
    ));
    let _conn_id = expect_accepted(&server, TIMEOUT);

    let reader_client = Arc::clone(&client);
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        reader_client.read(&mut buf) // blocks -- the server never sends anything
    });

    std::thread::sleep(Duration::from_millis(300)); // let the read actually become pending
    client.cancel();

    let result = reader.join().unwrap();
    assert!(
        matches!(result, Err(TransportError::Cancelled)),
        "expected Cancelled, got {result:?}"
    );

    drop(server);
}

/// A `SocketClient::write_all` blocked on one thread (the kernel send
/// buffer saturated because nobody drains it) is unblocked by `cancel()`
/// called from another.
#[test]
fn client_write_cancel_unblocks_from_another_thread() {
    if !run_isolated("client::client_write_cancel_unblocks_from_another_thread") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();
    let client = Arc::new(SocketClient::from_stream_for_test(
        UnixStream::connect(&path).unwrap(),
        0,
    ));
    let _conn_id = expect_accepted(&server, TIMEOUT);
    // Deliberately never drain `server.events()` from here on -- that is
    // what eventually stalls the server's reader and lets the raw socket
    // buffer fill up behind it, giving the client's own `write_all`
    // something real to block on.

    let writer_client = Arc::clone(&client);
    let writer = std::thread::spawn(move || {
        let payload = vec![0xCDu8; 65_536];
        loop {
            match writer_client.write_all(&payload) {
                Ok(()) => {}
                Err(e) => return e,
            }
        }
    });

    std::thread::sleep(Duration::from_secs(2)); // let the flood saturate the events channel + socket buffer
    client.cancel();

    let result = writer.join().unwrap();
    assert!(
        matches!(result, TransportError::Cancelled),
        "expected Cancelled, got {result:?}"
    );

    drop(server);
}

/// A SECOND concurrent same-direction `SocketClient::read` returns
/// `TransportError::ConcurrentSubmit` rather than racing the first caller.
#[test]
fn concurrent_same_direction_client_read_returns_distinct_error() {
    if !run_isolated("client::concurrent_same_direction_client_read_returns_distinct_error") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();
    let client = Arc::new(SocketClient::from_stream_for_test(
        UnixStream::connect(&path).unwrap(),
        0,
    ));
    let _conn_id = expect_accepted(&server, TIMEOUT);

    let a = Arc::clone(&client);
    let reader_a = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        a.read(&mut buf) // blocks -- nobody ever sends
    });

    // Review round fix (amended round 2): WAIT on the OBSERVED
    // precondition (A's read has genuinely ENTERED its critical section)
    // rather than a fixed sleep guessing at how long that takes -- a
    // PASSIVE flag read, never a `try_lock` that would itself momentarily
    // contend for the same slot A holds.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !client.read_slot_entered_for_test() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for A's read to genuinely enter the read slot"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let mut buf_b = [0u8; 16];
    let result_b = client.read(&mut buf_b);
    assert!(
        matches!(result_b, Err(TransportError::ConcurrentSubmit)),
        "expected ConcurrentSubmit, got {result_b:?}"
    );

    client.cancel();
    let result_a = reader_a.join().unwrap();
    assert!(
        matches!(result_a, Err(TransportError::Cancelled)),
        "expected Cancelled, got {result_a:?}"
    );

    drop(server);
}

/// Property 34: once cancelled, a `SocketClient` permanently rejects
/// EVERY later submission -- `read` and `write_all` alike -- without
/// ever touching the OS again. Cancelling twice is idempotent.
#[test]
fn cancelled_client_rejects_later_submissions() {
    if !run_isolated("client::cancelled_client_rejects_later_submissions") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();
    let client = SocketClient::from_stream_for_test(UnixStream::connect(&path).unwrap(), 0);
    let _conn_id = expect_accepted(&server, TIMEOUT);

    client.cancel();

    let mut buf = [0u8; 16];
    assert!(
        matches!(client.read(&mut buf), Err(TransportError::Cancelled)),
        "a cancelled client must permanently reject a later read"
    );
    assert!(
        matches!(client.write_all(b"x"), Err(TransportError::Cancelled)),
        "a cancelled client must permanently reject a later write"
    );

    // Idempotent: cancelling again must not panic or change the outcome.
    client.cancel();
    assert!(matches!(client.read(&mut buf), Err(TransportError::Cancelled)));

    drop(server);
}

/// ADR 0043 decision 7 (review round): a failed send latches the
/// connection closed, the same rule the server's own `writer_loop`
/// already follows — the peer (the server) closes after reading only
/// PART of a large write, so the client's own in-flight `write_all` fails
/// terminally partway through; THAT call returns its own real error
/// (never `Cancelled` — nobody called `cancel()`), but the latch it sets
/// means every LATER call on the SAME client is rejected with
/// `Cancelled`, exactly as if `cancel()` had been called.
#[test]
fn a_terminal_write_failure_latches_the_connection_closed() {
    if !run_isolated("client::a_terminal_write_failure_latches_the_connection_closed") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();
    let client = Arc::new(SocketClient::from_stream_for_test(
        UnixStream::connect(&path).unwrap(),
        0,
    ));
    let conn_id = expect_accepted(&server, TIMEOUT);

    // Data genuinely flows first -- a live connection, not one severed
    // before it ever carried a byte.
    client.write_all(&[0xABu8; 4096]).unwrap();
    match next_event(&server, TIMEOUT) {
        LaneEvent::Bytes(cid, bytes) => {
            assert_eq!(cid, conn_id, "Bytes for the wrong connection");
            assert!(!bytes.is_empty());
        }
        other => panic!("expected Bytes, got {other:?}"),
    }

    // Sever the connection server-side and WAIT for the server to report
    // it closed, so the peer is provably gone before the client writes
    // again. (The earlier shape severed underneath a blocking 8 MiB write
    // and could lose the race to a fast peer that drained the whole
    // payload first -- the rc.8 macOS flake, "got Ok(())".)
    server.close(conn_id);
    loop {
        match next_event(&server, TIMEOUT) {
            LaneEvent::Closed(cid, _) if cid == conn_id => break,
            LaneEvent::Bytes(cid, _) if cid == conn_id => continue,
            other => panic!("expected Closed, got {other:?}"),
        }
    }

    // Writing into a closed peer fails within a bounded number of chunks:
    // the kernel may absorb at most a socket buffer's worth before the
    // failure surfaces, never an unbounded amount.
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
        "expected the terminal write failure to surface as its OWN Io error \
         (never Cancelled -- nobody called cancel()), got {result:?}"
    );

    let mut buf = [0u8; 16];
    assert!(
        matches!(client.read(&mut buf), Err(TransportError::Cancelled)),
        "a client whose write already failed terminally must reject a later read"
    );
    assert!(
        matches!(client.write_all(b"x"), Err(TransportError::Cancelled)),
        "a client whose write already failed terminally must reject a later write"
    );

    drop(server);
}

/// ADR 0043 decision 27: with nothing ever listening at all (no socket
/// file exists), `connect(2)` sees `ENOENT` and fails on the FIRST
/// attempt -- no listener at all is the caller's to poll, never this
/// bound's to retry (an absent endpoint used to cost the full
/// [`CONNECT_BOUND`] even though no supervisor process existed yet).
/// Exercised through the fully public `connect_voyage_socket` -- since
/// connect never succeeds, the subsequent challenge step is never
/// reached, so this proves the CONNECT classification specifically, not
/// the challenge. Elapsed time is asserted GENEROUSLY (< 1 s) -- evidence
/// that the bound was never consumed, not a tight perf gate.
#[test]
#[cfg(target_os = "linux")]
fn connect_fails_fast_when_nothing_listens() {
    if !run_isolated("client::connect_fails_fast_when_nothing_listens") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();

    let started = Instant::now();
    let err = connect_voyage_socket(&id).unwrap_err();
    let elapsed = started.elapsed();

    // Codex review round finding 9: the real proof is the error
    // CLASSIFICATION below (a connect-family error, not a timeout or a
    // retried-then-gave-up outcome) -- the timing check is a loose sanity
    // bound against `CONNECT_BOUND` itself (the value the OLD retrying
    // behavior would have fully consumed), reported alongside it rather
    // than a tight wall-clock gate a busy runner could occasionally trip.
    eprintln!("connect_fails_fast_when_nothing_listens: elapsed={elapsed:?}");
    assert!(
        matches!(err, TransportError::Io { op, .. } if op.contains("connect")),
        "expected a connect-family error, got {err}"
    );
    assert!(
        elapsed < CONNECT_BOUND,
        "an absent endpoint (ENOENT) must fail on the FIRST attempt, never consume the full {CONNECT_BOUND:?}: took {elapsed:?}"
    );
}

/// ADR 0043 decision 27: a socket special file that exists but has
/// nobody listening behind it (bound, then the listener dropped without
/// unlinking) makes `connect(2)` see `ECONNREFUSED` -- no listener at
/// all, exactly like `ENOENT`, so it fails on the FIRST attempt too.
#[test]
#[cfg(target_os = "linux")]
fn connect_fails_fast_when_refused_by_a_stale_socket_file() {
    if !run_isolated("client::connect_fails_fast_when_refused_by_a_stale_socket_file") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    // `std`'s own `UnixListener` does not unlink its socket file on
    // `Drop` -- binding then immediately dropping leaves a stale special
    // file on disk with nobody behind it, so a real client's `connect(2)`
    // sees `ECONNREFUSED`, not `ENOENT`.
    drop(UnixListener::bind(&path).unwrap());

    let started = Instant::now();
    let err = connect_voyage_socket(&id).unwrap_err();
    let elapsed = started.elapsed();

    // Codex review round finding 9: same reasoning as
    // `connect_fails_fast_when_nothing_listens` -- classification is the
    // real proof, timing is a loose sanity bound reported alongside it.
    eprintln!("connect_fails_fast_when_refused_by_a_stale_socket_file: elapsed={elapsed:?}");
    assert!(
        matches!(err, TransportError::Io { op, .. } if op.contains("connect")),
        "expected a connect-family error, got {err}"
    );
    assert!(
        elapsed < CONNECT_BOUND,
        "ECONNREFUSED (no listener) must fail on the FIRST attempt, never consume the full {CONNECT_BOUND:?}: took {elapsed:?}"
    );
}

/// A single, raw, NONBLOCKING `connect(2)` attempt against `path` — used
/// only to saturate a real kernel listen backlog (Codex review round
/// finding 4). A BLOCKING `UnixStream::connect` against a full backlog
/// does not fail: on Linux, `connect(2)` on a stream socket with a full
/// backlog BLOCKS the calling thread until a slot frees rather than
/// returning an error — which would starve a saturation loop built out
/// of blocking connects of ever reaching the point where anything CAN
/// free a slot (a real hang, not merely a slow test, and exactly what
/// `[0..4096) { UnixStream::connect(..) }` risked). A nonblocking socket
/// instead returns `EAGAIN` immediately once the backlog is full —
/// `Ok(Some(stream))` is a queued connection (a slot was free);
/// `Ok(None)` is the full-backlog case this saturation loop is waiting
/// to observe; any other errno is a hard test-setup failure.
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
        let rc = libc::connect(raw, std::ptr::addr_of!(addr).cast(), addr_len);
        if rc == 0 {
            return Ok(Some(UnixStream::from_raw_fd(raw)));
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EAGAIN) {
            libc::close(raw);
            Ok(None)
        } else {
            libc::close(raw);
            Err(err)
        }
    }
}

/// ADR 0043 decision 27: `EAGAIN` (a full listen backlog) is a DIFFERENT
/// case from `ENOENT`/`ECONNREFUSED` -- it says nothing about whether a
/// listener exists, only that this attempt didn't finish -- and stays
/// retried within [`CONNECT_BOUND`]. A real listener with nothing ever
/// calling `accept` on it leaves the kernel-level backlog as the only
/// thing standing between a burst of raw connects and `EAGAIN`; this
/// drives enough concurrent connects to reliably fill it (observing a
/// real `EAGAIN`, not merely "some connects queued" — Codex review round
/// finding 4), then proves the retry loop still succeeds once a slot
/// frees (draining one queued connection via `accept()`) -- `EAGAIN`
/// alone must never become a fast, permanent failure the way
/// `ENOENT`/`ECONNREFUSED` now do.
#[test]
#[cfg(target_os = "linux")]
fn connect_retries_within_the_bound_on_a_full_backlog() {
    if !run_isolated("client::connect_retries_within_the_bound_on_a_full_backlog") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    // A real listener (not `SocketServer`, which spawns its own accept
    // loop) with nothing ever calling `accept()` on it -- every raw
    // connect piles up in the backlog until it's full.
    let listener = UnixListener::bind(&path).unwrap();
    // The test sets its own backlog: std's `bind` uses the host's maximum
    // (`somaxconn`), which would make the connects needed to fill it, and
    // so whether the open-file limit runs out first, a fact of the host.
    // `listen(2)` on a listening socket sets its backlog again.
    use std::os::unix::io::AsRawFd as _;
    const BACKLOG: i32 = 4;
    // SAFETY: `listener` owns a valid listening socket fd for this call.
    assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), BACKLOG) }, 0, "listen(2) must reset the backlog");

    // Saturate the backlog with NONBLOCKING raw connects until EAGAIN is
    // actually observed (a bounded attempt count so a failure to fill
    // cannot spin this test forever; the bound is a sanity cap on the
    // syscall count, not a race with the syscall itself, since a
    // nonblocking connect can never block).
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

    // `connect_voyage_socket` itself now races the saturated backlog: it
    // must not fail fast (this is EAGAIN, not "no listener"), and it must
    // eventually succeed once a slot frees -- draining ONE queued
    // connection via `accept()` frees exactly one backlog slot for it to
    // land in. `thread::scope` keeps `listener` alive by REFERENCE across
    // both the accept and the retry that follows it (Codex review round
    // finding 4: the previous version moved `listener` into the accept
    // thread's closure and so dropped it — closing the listening socket —
    // the INSTANT that one `accept()` returned, racing the retry loop's
    // own in-flight attempt; a retry landing after that drop sees a real
    // `ECONNREFUSED`, which decision 27 now fails FAST and fatally rather
    // than retrying).
    let (client, elapsed) = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(200));
            let _ = listener.accept();
        });
        let started = Instant::now();
        let client = connect_voyage_socket(&id).expect("expected the retry loop to succeed once a backlog slot freed");
        (client, started.elapsed())
    });

    eprintln!("connect_retries_within_the_bound_on_a_full_backlog: elapsed={elapsed:?}");
    assert!(
        elapsed < CONNECT_BOUND,
        "expected the retry to succeed comfortably inside {CONNECT_BOUND:?}, took {elapsed:?}"
    );
    drop(client);
    drop(listener);
    drop(saturating);
}

/// ADR 0043 decision 4: at capacity the acceptor accepts and closes
/// immediately -- from a real `SocketClient`'s own perspective (not a raw
/// `UnixStream`, see `capacity_excess_connection_is_closed_immediately`
/// above for that variant), the excess connection's first `read` sees an
/// early, ordinary `Ok(0)`.
#[test]
fn excess_connection_at_capacity_is_seen_as_early_eof() {
    if !run_isolated("client::excess_connection_at_capacity_is_seen_as_early_eof") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 1).unwrap();

    let _first = UnixStream::connect(&path).unwrap();
    let _first_conn = expect_accepted(&server, TIMEOUT);

    let second = SocketClient::from_stream_for_test(UnixStream::connect(&path).unwrap(), 0);
    let mut buf = [0u8; 16];
    let n = second
        .read(&mut buf)
        .expect("read should observe an ordered EOF, not an error");
    assert_eq!(n, 0, "expected the excess connection to see EOF promptly");

    drop(server);
}
