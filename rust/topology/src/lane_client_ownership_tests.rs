//! A remote attach over the host's link: one dial opens one channel and starts no login, and a channel the router
//! refuses is the dial's error with no second try.
use super::tests::{link_to, runtime};
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Topology 8: a remote attach is one dial, so one channel on the host's link and no login of its own.
#[cfg(unix)]
#[test]
fn a_remote_attach_opens_one_channel_and_starts_no_login() {
    use std::io::{BufRead, Read, Write};
    let peer = tempfile::Builder::new().prefix("sot-attach-peer-").tempdir_in("/tmp").expect("a private folder");
    let path = peer.path().join("d.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind the daemon stand-in");
    let rt = runtime();
    let made = Arc::new(AtomicUsize::new(0));
    let (_link, handle) = link_to(&rt, path, made.clone());
    let served = std::thread::spawn(move || {
        let (conn, _) = listener.accept().expect("the dial's connection");
        let mut reader = std::io::BufReader::new(conn.try_clone().unwrap());
        let mut conn = conn;
        let (mut hello, mut request) = (Vec::new(), Vec::new());
        reader.read_until(b'\n', &mut hello).unwrap();
        reader.read_until(b'\n', &mut request).unwrap();
        let answers = [
            Frame::res(1, op::HELLO, serde_json::json!({ "ok": true })),
            Frame::res(2, op::LANE_CONNECT, serde_json::json!({ "ok": true, "pid": std::process::id(), "created": 1 })),
        ];
        for answer in answers {
            let mut line = serde_json::to_vec(&answer).unwrap();
            line.push(b'\n');
            conn.write_all(&line).unwrap();
        }
        let mut rest = Vec::new();
        let _ = reader.read_to_end(&mut rest);
    });
    let endpoint = DaemonLaneEndpoint::new(LaneDial::Link(handle), None);
    let client = endpoint.dial("row-owned-peer").expect("the one channel answers");
    assert_eq!(made.load(Ordering::SeqCst), 1, "a remote attach opens exactly one channel");
    drop(client);
    sot_platform::test_wait::within(std::time::Duration::from_secs(10), "the stand-in", move || served.join()).unwrap();
    assert_eq!(made.load(Ordering::SeqCst), 1, "and nothing is opened behind it");
}

/// A channel the router could not connect is the dial's error, `Unreachable` with the router's words, and the dial
/// does not try again by itself.
#[test]
fn a_refused_channel_is_the_dials_error_with_no_second_try() {
    let rt = runtime();
    let made = Arc::new(AtomicUsize::new(0));
    let nowhere = std::env::temp_dir().join(format!("sot-no-daemon-{}", std::process::id())).join("d.sock");
    let (_link, handle) = link_to(&rt, nowhere, made.clone());
    let endpoint = DaemonLaneEndpoint::new(LaneDial::Link(handle), None);
    match endpoint.dial("row") {
        Err(TransportError::Unreachable(error)) => assert!(error.to_string().contains("unreachable"), "{error}"),
        other => panic!("expected the router's refusal, got {:?}", other.err()),
    }
    assert_eq!(made.load(Ordering::SeqCst), 1);
}
