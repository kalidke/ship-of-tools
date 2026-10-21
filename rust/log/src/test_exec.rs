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
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the writer");
        child.stdin.take().expect("the writer's stdin").write_all(body.as_ref()).expect("feed the writer");
        let (status, _, stderr) =
            sot_platform::test_isolated::drain(child).wait_within(sot_platform::test_isolated::ISOLATION_TIMEOUT);
        assert!(status.success(), "the writer failed for {}: {status}, stderr: {stderr}", path.display());
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod the program");
    }
    #[cfg(not(unix))]
    std::fs::write(path, body).expect("write the program");
}

#[cfg(test)]
#[path = "test_exec_tests.rs"]
mod tests;
