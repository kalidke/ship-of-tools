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
    for pin in [false, true] {
        let (tx, rx) = std::sync::mpsc::channel();
        let path = fifo.clone();
        let opener = std::thread::spawn(move || {
            tx.send(if pin { PinnedDir::open(&path).is_ok() } else { dir_identity(&path).is_ok() }).unwrap();
        });
        let outcome = rx.recv_timeout(std::time::Duration::from_secs(30));
        if outcome.is_err() {
            // Only when the open is waiting: a writer releases it, so the thread can be joined.
            let _writer = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo);
        }
        opener.join().unwrap();
        assert_eq!(outcome, Ok(false), "a FIFO is not a directory: refused at once (pin={pin})");
    }
}

#[test]
fn a_directory_pin_rejects_an_ordinary_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    std::fs::write(&file, "not a directory").unwrap();
    assert!(PinnedDir::open(&file).is_err());
}

/// The borrowed descriptor, unlike macOS's pinned_path, remains rooted in the object after an ancestor swap.
#[test]
fn a_borrowed_directory_pin_keeps_its_folder_after_an_ancestor_swap() {
    use std::io::Read;
    use std::os::fd::{AsFd, AsRawFd, FromRawFd};
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent");
    let row = parent.join("row");
    let other = dir.path().join("other");
    std::fs::create_dir_all(&row).unwrap();
    std::fs::create_dir_all(other.join("row")).unwrap();
    std::fs::write(row.join("page"), "original").unwrap();
    std::fs::write(other.join("row/page"), "replacement").unwrap();
    let pin = PinnedDir::open(&row).unwrap();
    let identity = pin.identity().unwrap();
    std::fs::rename(&parent, dir.path().join("moved")).unwrap();
    std::os::unix::fs::symlink(&other, &parent).unwrap();
    assert_ne!(dir_identity(&row).unwrap(), identity);
    let name = std::ffi::CString::new("page").unwrap();
    // SAFETY: the borrowed pin and name live through openat; the returned descriptor is owned immediately.
    let fd = unsafe { libc::openat(pin.as_fd().as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW) };
    assert!(fd >= 0, "{}", std::io::Error::last_os_error());
    // SAFETY: openat returned this fresh descriptor, owned by no other value.
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    assert_eq!(text, "original");
    assert_eq!(pin.identity().unwrap(), identity);
}
