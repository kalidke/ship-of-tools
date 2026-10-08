//! Test-only (feature `test-support`, Unix): a socket another OS account listens on, in a folder this test owns, for
//! the tests of ADR 0049's User isolation. A root helper (`sudo -n`, as on the hosted Linux and macOS runners) binds it
//! and listens as `nobody`. A test that cannot start one says so and passes, except on CI (`GITHUB_ACTIONS` set),
//! where that is a failure.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

/// The helper, run by `/usr/bin/python3` as root. It binds the socket path it is given (mode 0666, so the test's
/// account may connect), sets its effective uid to `nobody`'s, listens with a backlog of 1 (so the kernel records
/// `nobody` for the listener), and takes root back. With `full` it fills the backlog with connections it never
/// accepts, and fails if 64 do not fill it. It prints `ready` and waits for its input to end; then it accepts every
/// connection still queued, reads each to its end, removes the socket and prints the bytes it read (`open` when a
/// client was still open five seconds after the input ended). Its alarm ends it within two minutes whatever it waits on.
const LISTENER: &str = r#"
import os, pwd, signal, socket, sys
signal.alarm(120)
p, full = sys.argv[1], sys.argv[2] == "full"
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.bind(p)
os.chmod(p, 0o666)
os.seteuid(pwd.getpwnam("nobody").pw_uid)
s.listen(1)
os.seteuid(0)
held = []
while full:
    if len(held) == 64:
        sys.exit("the backlog never filled")
    h = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    h.setblocking(False)
    try:
        h.connect(p)
    except (BlockingIOError, ConnectionRefusedError):
        break
    held.append(h)
print("ready", flush=True)
sys.stdin.read()
for h in held:
    h.close()
s.setblocking(False)
n = 0
for _ in range(1000):
    try:
        a, _ = s.accept()
    except BlockingIOError:
        break
    except OSError:
        continue
    a.settimeout(5)
    try:
        while True:
            b = a.recv(4096)
            if not b:
                break
            n += len(b)
    except socket.timeout:
        print("open", flush=True)
        sys.exit(1)
    a.close()
os.unlink(p)
print(n, flush=True)
"#;

/// How long the helper may take to start, or to report when it ends.
const BOUND: Duration = Duration::from_secs(20);

/// Ends `child` within `BOUND`: closes its input and waits. `Some(status)` when it exited on its own; `None` when it was
/// still running at `BOUND`, or could not be polled. It is then killed (sudo; the root helper ends on its input's end or
/// its alarm), and its exit is not taken as confirmed. Never waits without a bound.
fn reap(child: &mut Child) -> Option<std::process::ExitStatus> {
    drop(child.stdin.take());
    let deadline = std::time::Instant::now() + BOUND;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                return None;
            }
        }
    }
}

/// A foreign-account listener in an atomically allocated directory owned by this fixture.
/// Tests close their clients before finish; an unconfirmed helper exit retains the directory.
pub struct ForeignListener {
    /// The socket's path.
    pub path: PathBuf,
    child: Option<Child>,
    lines: Receiver<String>,
    folder: PathBuf,
}

impl ForeignListener {
    /// Starts the helper, its backlog full when `full`. `None` when it cannot start here: the test then says so and
    /// passes, except on CI, where this panics.
    pub fn start(full: bool) -> Option<Self> {
        use std::os::unix::fs::PermissionsExt;
        // Keep the socket path short enough for macOS's sun_path.
        let allocated = tempfile::Builder::new()
            .prefix("sotfo-")
            .tempdir_in("/tmp")
            .expect("allocate the fixture directory atomically");
        std::fs::set_permissions(allocated.path(), std::fs::Permissions::from_mode(0o700))
            .expect("make the fixture directory private");
        // Relinquish automatic cleanup before a helper can use this directory.
        let folder = allocated.keep();
        let path = folder.join("s.sock");
        let spawned = Command::new("sudo")
            .args(["-n", "/usr/bin/python3", "-c", LISTENER])
            .arg(&path)
            .arg(if full { "full" } else { "open" })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                // No helper was started; this allocator-created directory is ours.
                std::fs::remove_dir_all(&folder).expect("remove the unused fixture directory");
                return skip(&format!("cannot start the listener helper: {error}"));
            }
        };
        let stdout = child.stdout.take().expect("the helper's stdout");
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        match lines.recv_timeout(BOUND) {
            Ok(line) if line == "ready" => Some(Self {
                path,
                child: Some(child),
                lines,
                folder,
            }),
            _ => match reap(&mut child) {
                Some(_) => {
                    std::fs::remove_dir_all(&folder).expect("remove the ended fixture directory");
                    skip("the helper started no listener")
                }
                None => panic!(
                    "listener startup failed; helper termination unconfirmed; directory retained"
                ),
            },
        }
    }

    /// Ends the helper and returns how many bytes it read from the connections queued on it. Linux hands over a queued
    /// connection whose client has closed, with what it sent; macOS may refuse that accept, so there such a connection
    /// counts 0, and a zero is no evidence that nothing was written.
    pub fn finish(mut self) -> u64 {
        drop(self.child.as_mut().expect("the owned helper").stdin.take());
        let count = self
            .lines
            .recv_timeout(BOUND)
            .expect("the helper's byte count");
        assert_ne!(
            count, "open",
            "a client of the helper stayed open after the test ended it"
        );
        let count = count.parse().expect("a byte count");
        let mut child = self.child.take().expect("the owned helper");
        let status = reap(&mut child)
            .unwrap_or_else(|| panic!("helper termination unconfirmed; directory retained"));
        std::fs::remove_dir_all(&self.folder).expect("remove the ended fixture directory");
        assert!(status.success(), "the listener helper failed: {status}");
        count
    }
}

impl Drop for ForeignListener {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        match reap(&mut child) {
            Some(_) => {
                if let Err(error) = std::fs::remove_dir_all(&self.folder) {
                    eprintln!("ended fixture directory cleanup failed: {error}");
                    assert!(
                        std::thread::panicking(),
                        "ended fixture directory cleanup failed: {error}"
                    );
                }
            }
            None => {
                eprintln!("helper termination unconfirmed; fixture directory retained");
                assert!(
                    std::thread::panicking(),
                    "helper termination unconfirmed; directory retained"
                );
            }
        }
    }
}

/// A test that cannot run here says so and passes, except on CI, where a silent skip is a failure to be seen.
fn skip<T>(reason: &str) -> Option<T> {
    eprintln!("skipped: {reason}");
    assert!(
        std::env::var_os("GITHUB_ACTIONS").is_none(),
        "a test skipped on CI: {reason}"
    );
    None
}
