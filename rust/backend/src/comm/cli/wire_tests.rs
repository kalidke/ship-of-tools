use super::*;

#[test]
fn a_refused_hello_is_the_daemons_words_and_silence_says_nothing_answered() {
    let ep = "unix:/run/x.sock";
    assert_eq!(said(ep, "unix:/run/x.sock: hello refused: a second OS account"), "hello refused: a second OS account");
    assert_eq!(said(ep, "unix:/run/x.sock: no reply to comm.file within 5s"), "the daemon did not answer at unix:/run/x.sock: no reply to comm.file within 5s");
    assert_eq!(said(ep, "connect: refused"), "the daemon did not answer at unix:/run/x.sock: connect: refused");
}

/// A daemon that accepts and never answers is a `FAILED` 5 s after the send began, whatever the inbox lock's wait.
#[cfg(unix)]
#[test]
fn a_silent_daemon_is_a_failure_at_five_seconds() {
    let _t = crate::comm::test_home::TestHome::new();
    std::env::set_var(crate::comm::mail::inbox::INBOX_LOCK_WAIT_ENV, "30");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("silent.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    // The connection is held open, unanswered, past the send's bound.
    std::thread::spawn(move || {
        let conn = listener.accept();
        std::thread::sleep(Duration::from_secs(8));
        drop(conn);
    });
    let started = std::time::Instant::now();
    let e = comm_file(&format!("unix:{}", path.display()), "me", "you", "x", false).unwrap_err();
    let took = started.elapsed();
    assert!(e.starts_with("the daemon did not answer at unix:"), "{e}");
    assert!(took >= FILE_WINDOW && took < FILE_WINDOW + Duration::from_millis(1500), "{took:?}");
}
