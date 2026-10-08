//! Test-only (feature `test-support`): the one way a test writes a program it will run. A short-lived child writes the
//! file, so the test process never holds it open for writing and no forked child can inherit that descriptor.

use std::path::Path;

/// Writes `body` to `path` and makes it executable. A process that still holds a program open for writing makes
/// `execve` of it fail with ETXTBSY; a test thread's fork copies every descriptor its process has open until the child
/// execs. So on Unix `/bin/cat` writes the file from a child of its own (which never forks), and the test process only
/// chmods it. Both programs are named by absolute path: no `PATH` read. Elsewhere there is no such failure.
pub fn write_executable(path: &Path, body: impl AsRef<[u8]>) {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use std::process::{Command, Stdio};
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(r#"exec /bin/cat > "$1""#)
            .arg("sh")
            .arg(path)
            .stdin(Stdio::piped())
            .spawn()
            .expect("spawn the writer");
        child.stdin.take().expect("the writer's stdin").write_all(body.as_ref()).expect("feed the writer");
        assert!(child.wait().expect("wait for the writer").success(), "the writer failed for {}", path.display());
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod the program");
    }
    #[cfg(not(unix))]
    std::fs::write(path, body).expect("write the program");
}

#[cfg(test)]
mod tests {
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
        if !crate::test_isolated::run_isolated(
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
}
