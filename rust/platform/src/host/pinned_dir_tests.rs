use super::*;

// A path that is not a directory fails at open: a FIFO never holds the
// caller waiting for a writer. A transcript can name any path as the
// directory it started in, and the daemon asks this function about it.
#[test]
fn a_fifo_fails_at_once_and_never_waits_for_a_writer() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("fifo");
    let c_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: `c_path` is a NUL-terminated path that lives through the call.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0, "mkfifo");
    let (tx, rx) = std::sync::mpsc::channel();
    let path = fifo.clone();
    let opener = std::thread::spawn(move || tx.send(dir_identity(&path).is_ok()).unwrap());
    let outcome = rx.recv_timeout(std::time::Duration::from_secs(30));
    if outcome.is_err() {
        // Only when the open is waiting: a writer releases it, so the
        // thread can be joined.
        let _writer = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo);
    }
    opener.join().unwrap();
    assert_eq!(
        outcome,
        Ok(false),
        "a FIFO is not a directory: refused, at once"
    );
}
