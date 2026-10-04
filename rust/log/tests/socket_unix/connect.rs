//! Connect, exchange and endpoint-guard tests: bytes both ways, multiplexed clients, the owner-only socket, the rival bind, capacity.

use super::*;

/// Pull `Bytes` events for `conn_id` until `expected_len` bytes have
/// accumulated — the stream is byte-type, so a single write is not
/// guaranteed to surface as a single `Bytes` event.
fn accumulate_bytes(
    server: &SocketServer,
    conn_id: ConnId,
    expected_len: usize,
    timeout: Duration,
) -> Vec<u8> {
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    while out.len() < expected_len {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "only got {} of {expected_len} expected bytes: {out:?}",
            out.len()
        );
        match next_event(server, remaining) {
            LaneEvent::Bytes(cid, bytes) => {
                assert_eq!(cid, conn_id, "Bytes for the wrong connection");
                out.extend(bytes);
            }
            other => panic!("expected Bytes, got {other:?}"),
        }
    }
    assert_eq!(
        out.len(),
        expected_len,
        "accumulated more than expected: {out:?}"
    );
    out
}

/// Test 1: one server, one client, bytes both ways (accumulated); a
/// marker-tagged send's `Sent` event fires once its `write` physically
/// completes.
#[test]
fn server_and_client_exchange_bytes_and_sent_carries_marker() {
    if !run_isolated("connect::server_and_client_exchange_bytes_and_sent_carries_marker") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 4).unwrap();
    let mut client = UnixStream::connect(&path).unwrap();
    let conn_id = expect_accepted(&server, TIMEOUT);

    let outbound = b"hello from client";
    client.write_all(outbound).unwrap();
    let got = accumulate_bytes(&server, conn_id, outbound.len(), TIMEOUT);
    assert_eq!(got, outbound);

    let inbound = b"hello from server";
    server.send(conn_id, inbound.to_vec(), Some(42)).unwrap();
    let mut buf = vec![0u8; inbound.len()];
    let mut got = 0;
    while got < buf.len() {
        got += client.read(&mut buf[got..]).unwrap();
    }
    assert_eq!(buf, inbound);

    match next_event(&server, TIMEOUT) {
        LaneEvent::Sent(cid, marker) => {
            assert_eq!(cid, conn_id);
            assert_eq!(marker, 42);
        }
        other => panic!("expected Sent, got {other:?}"),
    }

    drop(server);
}

/// Test 2: two clients connected to the same voyage socket are
/// multiplexed by distinct `ConnId`s.
#[test]
fn two_concurrent_clients_multiplexed_by_conn_id() {
    if !run_isolated("connect::two_concurrent_clients_multiplexed_by_conn_id") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 4).unwrap();

    let mut client_a = UnixStream::connect(&path).unwrap();
    let conn_a = expect_accepted(&server, TIMEOUT);
    let mut client_b = UnixStream::connect(&path).unwrap();
    let conn_b = expect_accepted(&server, TIMEOUT);
    assert_ne!(conn_a, conn_b);

    client_a.write_all(b"from A").unwrap();
    client_b.write_all(b"from B").unwrap();

    let mut a_got = Vec::new();
    let mut b_got = Vec::new();
    let deadline = Instant::now() + TIMEOUT;
    while a_got.len() < 6 || b_got.len() < 6 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "timed out: a={a_got:?} b={b_got:?}");
        match next_event(&server, remaining) {
            LaneEvent::Bytes(cid, bytes) if cid == conn_a => a_got.extend(bytes),
            LaneEvent::Bytes(cid, bytes) if cid == conn_b => b_got.extend(bytes),
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert_eq!(a_got, b"from A");
    assert_eq!(b_got, b"from B");

    drop(server);
}

/// ADR 0043 decision 3: the socket file is owner-only (0600) inside a
/// private, owner-only (0700) runtime dir — the Unix analogue of
/// `pipe_win.rs`'s own SDDL descriptor test.
#[test]
fn socket_is_owner_only_in_a_private_dir() {
    if !run_isolated("connect::socket_is_owner_only_in_a_private_dir") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 1).unwrap();

    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = std::fs::symlink_metadata(&path).expect("stat the socket file");
    assert!(!meta.file_type().is_symlink(), "socket file must not be a symlink");
    assert_eq!(
        meta.permissions().mode() & 0o777,
        0o600,
        "socket file must be owner-only 0600"
    );
    assert_eq!(meta.uid(), current_uid());

    let parent = path.parent().expect("socket path has a parent");
    let parent_meta = std::fs::symlink_metadata(parent).expect("stat the runtime dir");
    assert!(!parent_meta.file_type().is_symlink());
    assert_eq!(
        parent_meta.permissions().mode() & 0o777,
        0o700,
        "runtime dir must be owner-only 0700"
    );
    assert_eq!(parent_meta.uid(), current_uid());

    drop(server);
}

/// ADR 0043 decision 2, property 3/5: a rival RAW `UnixListener::bind` on
/// the SAME path must fail `EADDRINUSE` while the server is live (the
/// name is held continuously), and succeed ONLY after
/// `disconnect_listener` — proving the name is freed SYNCHRONOUSLY, not
/// merely eventually. A second `SocketServer::bind` is deliberately NOT
/// the probe here (it would itself unlink-and-rebind, telling us nothing
/// about whether the FIRST server was still actually holding the name).
#[test]
fn rival_bind_fails_while_held_and_succeeds_after_disconnect_listener() {
    if !run_isolated("connect::rival_bind_fails_while_held_and_succeeds_after_disconnect_listener") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let mut server = SocketServer::bind(&id, 2).unwrap();

    let err = UnixListener::bind(&path).expect_err("expected the held name to refuse a rival bind");
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::AddrInUse,
        "expected EADDRINUSE while the server is live, got {err}"
    );

    server.disconnect_listener();
    UnixListener::bind(&path)
        .unwrap_or_else(|e| panic!("expected the freed name to bind again: {e}"));

    // Codex review finding 1 (P1): `disconnect_listener` must unlink its
    // OWN endpoint EXACTLY ONCE. Bind a REPLACEMENT `SocketServer` at the
    // identical voyage id (as a real caller's next leg would), then drop
    // the OLD (already torn-down) server — its `Drop` calls
    // `disconnect_listener` again. If that second call unlinked
    // unconditionally, it would delete the REPLACEMENT's endpoint out
    // from under it: prove it did not by connecting a fresh client to the
    // replacement and observing `Accepted`, and by confirming its socket
    // file still exists.
    let mut replacement = SocketServer::bind(&id, 2).unwrap();
    drop(server);

    let _client = UnixStream::connect(&path)
        .unwrap_or_else(|e| panic!("replacement server should still accept connections: {e}"));
    expect_accepted(&replacement, TIMEOUT);
    assert!(
        path.exists(),
        "the replacement's own socket file must still exist after the old server's Drop"
    );

    replacement.disconnect_listener();
}

/// ADR 0043 decision 4: Unix cannot refuse a connection at connect time —
/// the kernel completes the handshake from the listen backlog regardless
/// of `max_connections` — so at capacity the acceptor accepts and closes
/// immediately. The excess client sees EOF promptly; the first
/// (already-registered) connection is unaffected.
#[test]
fn capacity_excess_connection_is_closed_immediately() {
    if !run_isolated("connect::capacity_excess_connection_is_closed_immediately") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 1).unwrap();

    let first = UnixStream::connect(&path).unwrap();
    let _first_conn = expect_accepted(&server, TIMEOUT);

    let mut second = UnixStream::connect(&path).unwrap();
    second
        .set_read_timeout(Some(TIMEOUT))
        .expect("set_read_timeout");
    let mut buf = [0u8; 16];
    let n = second
        .read(&mut buf)
        .expect("read should observe an ordered EOF, not an error");
    assert_eq!(n, 0, "expected the excess connection to see EOF promptly");

    // No event is ever queued for the excess connection, and the first
    // (still-live, at-capacity) connection is unaffected.
    assert!(
        server.events().recv_timeout(Duration::from_millis(200)).is_err(),
        "no event expected: the excess connection never registers, and the first stays live"
    );

    drop(first);
    drop(second);
    drop(server);
}
