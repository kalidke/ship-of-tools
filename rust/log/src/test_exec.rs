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

    /// The test process never holds a written program open: its only descriptor on the file is the reader's. A FIFO
    /// lets the test look while the writer is still writing: the writer's `open` returns when the reader's does, and
    /// the writer cannot finish until the reader drains it.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_test_process_never_holds_a_written_program_open() {
        use std::io::Read;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("program");
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: a plain mkfifo(3) of a path in this test's own folder.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let writer = std::thread::spawn({
            let path = path.clone();
            move || write_executable(&path, vec![b'#'; 1 << 20])
        });
        // A non-blocking open succeeds with no writer, so a helper that never opens the path fails the wait below
        // instead of hanging the test.
        let mut reader = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&path).unwrap();
        let mut first = Vec::new();
        let mut chunk = [0u8; 4096];
        let give_up = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while first.len() < 4096 {
            assert!(std::time::Instant::now() < give_up, "the helper never opened the program for writing");
            match reader.read(&mut chunk) {
                Ok(n) if n > 0 => first.extend_from_slice(&chunk[..n]),
                _ => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
        let holders = std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .flatten()
            .filter(|fd| std::fs::read_link(fd.path()).is_ok_and(|link| link == path))
            .count();
        assert_eq!(holders, 1, "this process holds {holders} descriptors on the program; only the reader's is allowed");
        // SAFETY: clears O_NONBLOCK on this test's own reader so the drain below blocks until the writer is done.
        unsafe {
            let fd = std::os::fd::AsRawFd::as_raw_fd(&reader);
            libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) & !libc::O_NONBLOCK);
        }
        std::io::copy(&mut reader, &mut std::io::sink()).unwrap();
        writer.join().unwrap();
    }

    /// A program a test runs is made executable only here. Hits that are not programs (directory and socket modes, mode
    /// bits read or asserted, production checks) are allowed by file, exact line and count. Blind spot: an `fs::copy` of an
    /// executable makes a program without any of these words; a reviewer checks for it.
    #[test]
    fn only_test_exec_makes_a_test_program_executable() {
        const WORDS: [&str; 8] = ["0o755", "0o775", "0o777", "0o0755", "0o111", "chmod 7", "chmod +x", ".mode(0o7"];
        const ALLOWED: &[(&str, &str, usize)] = &[
            ("rust/backend/src/agents/folder_trust.rs", "let mode = std::fs::metadata(path).map(|m| m.permissions().mode() & 0o777).unwrap_or(0o600);", 1),
            ("rust/backend/src/paths.rs", ".mode(0o700)", 1),
            ("rust/backend/src/paths.rs", "assert_eq!(meta.permissions().mode() & 0o777, 0o700);", 1),
            ("rust/backend/src/paths.rs", "assert_eq!(std::fs::metadata(&leaf).unwrap().permissions().mode() & 0o777, 0o700);", 1),
            ("rust/backend/src/paths.rs", "meta.permissions().mode() & 0o777,", 1),
            ("rust/backend/src/rows/spawn/detach.rs", ".map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)", 1),
            ("rust/backend/src/sidecars/julia.rs", "Ok(md) if md.permissions().mode() & 0o111 == 0 => Some(\"not executable\"),", 1),
            ("rust/backend/tests/stdio_bridge.rs", "match std::fs::DirBuilder::new().mode(0o700).create(&cur) {", 1),
            ("rust/backend/tests/topology_set.rs", "let mode = std::fs::metadata(&path).expect(\"the hub's comm path\").permissions().mode() & 0o7777;", 1),
            ("rust/backend/tests/window_start.rs", "let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));", 1),
            ("rust/frontend/src/lease_grant_tests.rs", "std::fs::DirBuilder::new().mode(0o700).create(&dir).expect(\"private folder\");", 1),
            ("rust/frontend/src/pages.rs", "assert_eq!(std::fs::metadata(f).unwrap().permissions().mode() & 0o777, 0o600, \"{f:?}\");", 1),
            ("rust/frontend/src/net/transport/tests.rs", "std::fs::DirBuilder::new().mode(0o700).create(&dir).expect(\"private folder\");", 1),
            ("rust/log/src/host/durable.rs", "std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o111)).unwrap();", 1),
            ("rust/log/src/host/state_dir.rs", "std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();", 1),
            ("rust/log/src/host/state_dir.rs", "std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();", 1),
            ("rust/log/src/lane/socket_unix/listener.rs", ".mode(0o700)", 1),
            ("rust/log/src/lane/socket_unix/listener.rs", "|| st.st_mode & 0o777 != 0o600", 1),
            ("rust/log/src/store/recovery.rs", "std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o111)).unwrap();", 1),
            ("rust/log/tests/connect_own.rs", "let (dir, path, listener) = listener_in_folder(0o755);", 1),
            ("rust/log/tests/socket_unix/connect.rs", "meta.permissions().mode() & 0o777,", 1),
            ("rust/log/tests/socket_unix/connect.rs", "parent_meta.permissions().mode() & 0o777,", 1),
        ];
        let mut found = Vec::new();
        for (rel, text) in crate::test_scan::rust_sources() {
            if rel == "rust/log/src/test_exec.rs" {
                continue;
            }
            let mut used = std::collections::HashMap::new();
            for (n, line) in text.lines().enumerate() {
                if !WORDS.iter().any(|w| line.contains(w)) {
                    continue;
                }
                let seen = used.entry(line.trim().to_string()).or_insert(0usize);
                *seen += 1;
                let room = ALLOWED.iter().find(|(f, l, _)| *f == rel && *l == line.trim()).map_or(0, |(_, _, c)| *c);
                if *seen > room {
                    found.push(format!("{rel}:{}: {}", n + 1, line.trim()));
                }
            }
        }
        assert!(found.is_empty(), "a program is made executable outside write_executable:\n{}", found.join("\n"));
    }
}
