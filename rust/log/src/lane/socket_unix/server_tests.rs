use super::*;

#[test]
fn transport_progresses_while_recorder_is_busy() {
    let test = "lane::socket_unix::server::tests::transport_progresses_while_recorder_is_busy";
    if !sot_platform::test_isolated::run_isolated(test) {
        return;
    }
    let root = tempfile::Builder::new()
        .prefix("sot-busy-")
        .tempdir_in("/tmp")
        .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("SOT_RUNTIME_DIR", root.path());
    let id = uuid::Uuid::now_v7().to_string();
    let server = SocketServer::bind(&id, 1).unwrap();
    let path = voyage_socket_path(&id).unwrap();
    let before = server.progress_for_test().skipped;
    let held = server.hold_progress_for_test();
    let (done, observed) = mpsc::channel();
    let producer = thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            use std::io::{Read, Write};
            let deadline = Instant::now() + Duration::from_secs(5);
            let event = || {
                server
                    .events()
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .unwrap()
            };
            let mut client = UnixStream::connect(path).unwrap();
            let LaneEvent::Accepted(conn) = event() else {
                panic!("expected Accepted");
            };
            client
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            client
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            client.write_all(b"a").unwrap();
            assert!(matches!(event(), LaneEvent::Bytes(_, ref bytes) if bytes == b"a"));
            server.send(conn, b"b".to_vec(), Some(7)).unwrap();
            let mut byte = [0];
            client.read_exact(&mut byte).unwrap();
            assert_eq!(byte, *b"b");
            assert!(matches!(event(), LaneEvent::Sent(_, 7)));
            server.close(conn);
            assert!(matches!(
                event(),
                LaneEvent::Closed(_, ClosedReason::Closed)
            ));
            assert!(server.progress_for_test().skipped > before);
        }));
        done.send(result.is_ok()).unwrap();
        (server, result)
    });
    let completed_while_held = observed.recv_timeout(Duration::from_secs(6));
    drop(held); // Always release before joining or ordinary server cleanup, including a red.
    let (server, result) = producer.join().unwrap();
    drop(server);
    assert!(
        completed_while_held == Ok(true),
        "transport stopped while recorder was busy"
    );
    result.unwrap();
    eprintln!("recorder-proof test={test} progress=while-held skipped=increased bodies=1");
}
