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

/// Marks the open `file` for deletion, or takes the mark back. While the mark stands and the handle is open, every open
/// of the file's name answers access denied (the kernel's delete-pending status).
#[cfg(windows)]
fn mark_for_deletion(file: &std::fs::File, mark: bool) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO};
    let info = FILE_DISPOSITION_INFO { DeleteFile: mark as _ };
    let size = size_of::<FILE_DISPOSITION_INFO>() as u32;
    // SAFETY: an open handle and a structure of the size passed.
    let ok = unsafe { SetFileInformationByHandle(file.as_raw_handle() as _, FileDispositionInfo, &info as *const _ as *const _, size) };
    assert_ne!(ok, 0, "{}", std::io::Error::last_os_error());
}

/// A file whose name refuses every open with access denied (status delete pending) until `mark_for_deletion(.., false)`,
/// and the handle that keeps it so.
#[cfg(windows)]
fn refused_file(dir: &Path) -> (std::path::PathBuf, std::fs::File) {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, STATUS_DELETE_PENDING};
    use windows_sys::Win32::Storage::FileSystem::DELETE;
    let path = dir.join("record");
    std::fs::write(&path, "text").unwrap();
    let held = std::fs::OpenOptions::new().access_mode(DELETE).open(&path).unwrap();
    mark_for_deletion(&held, true);
    last_nt::clear();
    let refused = std::fs::File::open(&path).unwrap_err();
    assert_eq!(refused.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));
    assert_eq!(last_nt::status(), STATUS_DELETE_PENDING);
    (path, held)
}

#[cfg(windows)]
#[test]
fn a_name_refused_for_a_moment_is_read_once_it_answers() {
    // The name refuses every open with access denied and a status that says the file is going, the answer an open gets at
    // the instant its name is replaced. The read is refused once and held there, the refusal is ended, and the read
    // answers with the file. With no second ask the read has answered the refusal (os error 5) before anything ends it.
    let dir = tempfile::tempdir().unwrap();
    let (path, held) = refused_file(dir.path());
    let arm = crate::test_hold::Holds::here().arm("platform.regular.refused").key(path.display()).hold(1);
    std::thread::scope(|sc| {
        let read = sc.spawn(|| read_regular(&path, 16).map_err(|e| e.raw_os_error()));
        let began = std::time::Instant::now();
        while arm.arrivals() == 0 && !read.is_finished() && began.elapsed() < crate::test_hold::WAIT {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        mark_for_deletion(&held, false);
        if arm.arrivals() > 0 {
            arm.release(0);
        }
        assert_eq!(read.join().unwrap(), Ok("text".to_string()));
    });
}

#[cfg(windows)]
#[test]
fn a_name_refused_past_the_window_answers_the_refusal() {
    use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
    let dir = tempfile::tempdir().unwrap();
    let (path, held) = refused_file(dir.path());
    let began = std::time::Instant::now();
    let refused = read_regular(&path, 16).unwrap_err();
    assert_eq!(refused.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));
    assert!(began.elapsed() >= std::time::Duration::from_millis(crate::host::RETRY_DEADLINE_MS), "answered after {:?}", began.elapsed());
    mark_for_deletion(&held, false);
    assert_eq!(read_regular(&path, 16).unwrap(), "text");
}

#[cfg(windows)]
#[test]
fn a_denial_that_is_not_a_name_being_replaced_is_answered_without_asking_again() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION};
    let dir = tempfile::tempdir().unwrap();
    // A folder answers access denied with a status of its own (a folder is not a file).
    let arm = crate::test_hold::Holds::here().arm("platform.regular.refused").key(dir.path().display()).hold(0);
    let refused = open_regular(dir.path()).unwrap_err();
    assert_eq!(refused.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));
    assert_eq!(arm.arrivals(), 0, "a folder was asked again");
    drop(arm);
    // A file another handle holds without sharing answers a sharing violation.
    let path = dir.path().join("record");
    std::fs::write(&path, "text").unwrap();
    let _held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&path).unwrap();
    let arm = crate::test_hold::Holds::here().arm("platform.regular.refused").key(path.display()).hold(0);
    let refused = open_regular(&path).unwrap_err();
    assert_eq!(refused.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32));
    assert_eq!(arm.arrivals(), 0, "a sharing violation was asked again");
}
