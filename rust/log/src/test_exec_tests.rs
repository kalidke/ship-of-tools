use super::*;

#[cfg(target_os = "linux")]
trait Finished {
    fn finished(&self) -> bool;
}
#[cfg(target_os = "linux")]
impl<T> Finished for std::thread::JoinHandle<T> {
    fn finished(&self) -> bool {
        self.is_finished()
    }
}

/// The test process never holds a written program open: its only descriptor on the file is the reader's. A FIFO
/// lets the test look while the writer is still writing: the writer's `open` returns when the reader's does, and
/// the writer cannot finish until the reader drains it.
#[cfg(target_os = "linux")]
#[test]
fn the_test_process_never_holds_a_written_program_open() {
    if !sot_platform::test_isolated::run_isolated(
        "test_exec::tests::the_test_process_never_holds_a_written_program_open",
    ) {
        return;
    }
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap().join("program");
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: a plain mkfifo(3) of a path in this test's own folder.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    let writer = std::thread::spawn({
        let path = path.clone();
        move || write_executable(&path, vec![b'#'; 1 << 20])
    });
    // A helper thread opens the reader, which blocks until the writer opens; a helper that never opens the path
    // fails the wait below instead of hanging the test.
    let (tx, rx) = std::sync::mpsc::channel();
    let opening = std::thread::spawn({
        let path = path.clone();
        move || tx.send(std::fs::File::open(&path).unwrap())
    });
    let mut reader = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap_or_else(|_| panic!("the helper never opened the program for writing"));
    let mut first = [0u8; 4096];
    reader.read_exact(&mut first).unwrap();
    let holders = std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .flatten()
        .filter(|fd| std::fs::read_link(fd.path()).is_ok_and(|link| link == path))
        .count();
    // Retain the observation, then finish the FIFO before raising its descriptor assertion.
    let draining = std::thread::spawn(move || std::io::copy(&mut reader, &mut std::io::sink()));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    for thread in [
        &opening as &dyn Finished,
        &writer as &dyn Finished,
        &draining as &dyn Finished,
    ] {
        while !thread.finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(thread.finished(), "FIFO fixture completion unconfirmed");
    }
    opening.join().unwrap().unwrap();
    let drained = draining.join().unwrap().unwrap();
    writer.join().unwrap();
    assert_eq!(
        drained + first.len() as u64,
        1 << 20,
        "FIFO writer not fully drained"
    );
    eprintln!("descriptor-fixture bodies=1 writer=active cleanup=confirmed holders={holders}");
    assert_eq!(
        holders, 1,
        "this process holds {holders} descriptors on the program; only the reader's is allowed"
    );
}
