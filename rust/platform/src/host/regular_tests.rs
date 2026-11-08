use super::*;
use std::io::ErrorKind;

#[test]
fn a_regular_file_reads_up_to_the_cap_and_no_further() {
    let dir = tempfile::tempdir().unwrap();
    let file = |name: &str, len: usize| {
        let p = dir.path().join(name);
        std::fs::write(&p, vec![b'#'; len]).unwrap();
        p
    };
    assert_eq!(read_regular(&file("at", 16), 16).unwrap().len(), 16);
    assert_eq!(
        read_regular(&file("over", 17), 16).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    assert_eq!(
        read_regular(&dir.path().join("absent"), 16)
            .unwrap_err()
            .kind(),
        ErrorKind::NotFound
    );
    // A directory is refused on every platform: on Unix by the handle check, on Windows by the open.
    assert!(read_regular(dir.path(), 16).is_err());
    #[cfg(unix)]
    assert_eq!(
        read_regular(dir.path(), 16).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    // Each refusal says which it is.
    let non_text = file("utf", 0);
    std::fs::write(&non_text, [0xff, 0xfe]).unwrap();
    let non_text = read_regular(&non_text, 16).unwrap_err();
    let big = read_regular(&file("big", 17), 16).unwrap_err();
    assert_eq!(non_text.kind(), ErrorKind::InvalidData);
    assert_ne!(non_text.to_string(), big.to_string());
}

#[cfg(unix)]
#[test]
fn a_planted_fifo_or_device_is_refused_without_a_wait() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("fifo");
    let c_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: a plain mkfifo(3) of a path in this test's own folder.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    // The read runs on its own thread so a read that blocks fails this test instead of hanging the suite.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_regular(&fifo, 16));
    });
    let got = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the read of a FIFO blocked");
    assert_eq!(got.unwrap_err().kind(), ErrorKind::InvalidInput);
    // The open alone answers the same, on the handle's kind.
    let e = open_regular(&dir.path().join("fifo")).unwrap_err();
    assert_eq!((e.kind(), e.to_string().as_str()), (ErrorKind::InvalidInput, "not a regular file"));
    // A device that never ends is not a regular file either.
    assert_eq!(
        read_regular(Path::new("/dev/zero"), 16).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(open_regular(Path::new("/dev/zero")).unwrap_err().kind(), ErrorKind::InvalidInput);
}
