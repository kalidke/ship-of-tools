/// A daemon that accepts and never answers the hello is an error at the dial's own bound, naming the endpoint and
/// the op.
#[test]
fn a_silent_daemon_is_an_error_at_the_bound() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let _held = std::thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(20));
        drop(s);
    });
    let (tx, rx) = std::sync::mpsc::channel();
    let ep = format!("unix:{}", path.display());
    let began = std::time::Instant::now();
    std::thread::spawn(move || {
        let _ = tx.send(super::dial_and_call(&ep, "probe-host", "version.query", serde_json::json!({}), std::time::Duration::from_secs(1)));
    });
    let result = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap_or_else(|_| panic!("dial_and_call had not returned after 10s"));
    let err = result.expect_err("a daemon that never answers");
    println!("dial-bound: {err} after {:?}", began.elapsed());
    assert!(err.ends_with(": no reply to version.query within 1s") && err.starts_with("unix:"), "{err}");
    assert!(began.elapsed() < std::time::Duration::from_secs(3), "{:?}", began.elapsed());
}
