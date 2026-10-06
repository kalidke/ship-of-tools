//! The owned piped lane child: I/O diagnosis, cancellation and teardown.

use std::sync::atomic::{AtomicBool, Ordering};
use sot_log::lane::client::Client;
use sot_log::lane::transport::TransportError;

/// The `Ssh` dial's client: a spawned `ssh … sotd stdio-bridge`
/// child whose stdin/stdout carry the lane bridge's own frames.
/// `ChildStdout`/`ChildStdin` are converted to `File` through `OwnedFd`
/// (unix) / `OwnedHandle` (windows) at construction, so `read`/
/// `write_all` go through `&self`.
///
/// A pipe has no `set_read_timeout`, so `cancel()` sets the flag and
/// **kills the child**; the kill closes the child's stdout, which EOFs a
/// parked read on both platforms. Final teardown polls reaping within a
/// two-second budget and reports an unconfirmed child end.
///
/// No new trust claim: `DaemonLaneEndpoint`'s own doc already states
/// that it holds no kernel handle on the peer and that every identity
/// claim traces to the daemon's own observation.
pub(super) struct BridgedClient {
    id: u32,
    #[cfg(test)]
    faults: std::sync::Mutex<ChildFaults>,
    #[cfg(test)]
    diagnostics: std::sync::Mutex<Vec<String>>,
    pub(super) child: std::sync::Mutex<std::process::Child>,
    pub(super) out: std::fs::File,
    pub(super) inp: std::fs::File,
    cancelled: AtomicBool,
    /// The child's last non-empty stderr line, kept by a drainer thread
    /// spawned at construction — ssh's own complaint ("Permission
    /// denied", or `unrecognised argument: --host` from a hub whose
    /// `sotd` predates C1) is the diagnosis a caller surfaces on
    /// failure, the same rule `stdio_bridge.rs` already sets for the far
    /// end.
    pub(super) last_stderr: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl BridgedClient {
    pub(super) fn spawn(recipe: &crate::topology::ssh_bridge::SshRecipe, gate: &crate::topology::ssh_bridge::LinkGate) -> Result<Self, TransportError> {
        #[allow(clippy::disallowed_methods, reason = "the lane dial's ssh, owned by the window's attach client")]
        let spawned = gate.spawn_sync(recipe);
        match spawned {
            Ok(child) => Self::wrap(child).map_err(TransportError::Unreachable),
            Err(crate::topology::ssh_bridge::SpawnError::LinkDown) => Err(TransportError::LinkDown),
            Err(crate::topology::ssh_bridge::SpawnError::Io(e)) => Err(TransportError::Unreachable(e)),
        }
    }

    /// The shared construction path — real `ssh` child ([`spawn`] above)
    /// or, in tests, any other piped-stdio child that stands in for one
    /// (so `cancel()`'s kill→EOF property is exercised without a real
    /// `ssh` on `PATH`).
    pub(super) fn wrap(mut child: std::process::Child) -> std::io::Result<Self> {
        let stdin = child.stdin.take().expect("spawned with a piped stdin");
        let stdout = child.stdout.take().expect("spawned with a piped stdout");
        let stderr = child.stderr.take().expect("spawned with a piped stderr");

        let last_stderr = std::sync::Arc::new(std::sync::Mutex::new(None));
        {
            let last_stderr = std::sync::Arc::clone(&last_stderr);
            std::thread::spawn(move || {
                use std::io::BufRead;
                let reader = std::io::BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    if !line.trim().is_empty() {
                        if let Ok(mut guard) = last_stderr.lock() {
                            *guard = Some(line);
                        }
                    }
                }
            });
        }

        #[cfg(unix)]
        let (inp, out) = {
            use std::os::fd::OwnedFd;
            (std::fs::File::from(OwnedFd::from(stdin)), std::fs::File::from(OwnedFd::from(stdout)))
        };
        #[cfg(windows)]
        let (inp, out) = {
            use std::os::windows::io::OwnedHandle;
            (std::fs::File::from(OwnedHandle::from(stdin)), std::fs::File::from(OwnedHandle::from(stdout)))
        };

        Ok(Self { id: child.id(), #[cfg(test)] faults: Default::default(), #[cfg(test)] diagnostics: Default::default(), child: std::sync::Mutex::new(child), out, inp, cancelled: AtomicBool::new(false), last_stderr })
    }

    pub(super) fn exited(&self) -> bool {
        match self.child.lock() {
            Ok(mut child) => !matches!(self.observe_exit(&mut child), Ok(None)),
            Err(_) => true,
        }
    }

    fn terminate(&self, child: &mut std::process::Child) -> std::io::Result<()> {
        #[cfg(test)] {
            let faults = self.faults.lock().unwrap();
            if faults.fail_terminate { return Err(std::io::Error::other("injected termination failure")); }
            if faults.hold_termination { return Ok(()); }
        }
        child.kill()
    }

    fn observe_exit(&self, child: &mut std::process::Child) -> std::io::Result<Option<std::process::ExitStatus>> {
        #[cfg(test)]
        if self.faults.lock().unwrap().fail_reap { return Err(std::io::Error::other("injected reap failure")); }
        child.try_wait()
    }

    fn report(&self, error: &std::io::Error) {
        eprintln!("lane child teardown failed: {error}");
        #[cfg(test)]
        self.diagnostics.lock().unwrap().push(format!("lane child teardown failed: {error}"));
    }

    fn failure(&self, operation: &str, error: impl std::fmt::Display) -> std::io::Error {
        std::io::Error::other(format!("{operation} owned child {}: {error}", self.id))
    }

    fn lock_child(&self, deadline: std::time::Instant) -> std::io::Result<std::sync::MutexGuard<'_, std::process::Child>> {
        loop {
            if std::time::Instant::now() >= deadline { return Err(self.failure("deadline", "child lock acquisition expired")); }
            match self.child.try_lock() {
                Ok(child) => return Ok(child),
                Err(std::sync::TryLockError::Poisoned(error)) => return Ok(error.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
    }

    fn request_termination(&self, child: &mut std::process::Child) -> std::io::Result<()> {
        if self.observe_exit(child).map_err(|error| self.failure("reap", error))?.is_some() { return Ok(()); }
        if let Err(error) = self.terminate(child) {
            // A kill racing a confirmed natural exit is benign; an unconfirmed exit is not.
            if self.observe_exit(child).is_ok_and(|exit| exit.is_some()) { return Ok(()); }
            return Err(self.failure("terminate", error));
        }
        Ok(())
    }

    fn teardown_inner(&self, deadline: std::time::Instant) -> std::io::Result<()> {
        let mut child = self.lock_child(deadline)?;
        self.request_termination(&mut child)?;
        loop {
            if self.observe_exit(&mut child).map_err(|error| self.failure("reap", error))?.is_some() { return Ok(()); }
            if std::time::Instant::now() >= deadline { return Err(self.failure("deadline", "2 s teardown budget expired")); }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    pub(super) fn teardown(&self) -> std::io::Result<()> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        self.cancelled.store(true, Ordering::SeqCst);
        self.teardown_inner(deadline).inspect_err(|error| self.report(error))
    }

    /// A short bounded poll for the child's last stderr line (this is the
    /// error path only, never the hot path). Stdout and stderr are
    /// separate pipes with no ordering guarantee between them, so a
    /// child that writes a diagnosis to stderr and closes stdout in the
    /// same instant can otherwise be observed here before its line
    /// lands.
    pub(super) fn poll_last_stderr(&self) -> Option<String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            if let Some(line) = self.last_stderr.lock().ok().and_then(|g| g.clone()) {
                return Some(line);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// The io error, with the child's last stderr line after it when one was captured: a dead child's own "Permission
    /// denied" explains the generic "broken pipe" its closed pipe leaves behind, and the error keeps its own words.
    pub(super) fn diagnose(&self, source: std::io::Error) -> std::io::Error {
        match self.poll_last_stderr() {
            Some(line) => std::io::Error::new(source.kind(), format!("{source}: {line}")),
            None => source,
        }
    }
}

impl Client for BridgedClient {
    fn write_all(&self, bytes: &[u8]) -> Result<(), TransportError> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(TransportError::Cancelled);
        }
        use std::io::Write;
        (&self.inp).write_all(bytes).map_err(|source| {
            if self.cancelled.load(Ordering::SeqCst) {
                TransportError::Cancelled
            } else {
                TransportError::Io { op: "lane write", source: self.diagnose(source) }
            }
        })
    }

    fn read(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        use std::io::Read;
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(TransportError::Cancelled);
        }
        (&self.out).read(buf).map_err(|source| {
            if self.cancelled.load(Ordering::SeqCst) {
                TransportError::Cancelled
            } else {
                TransportError::Io { op: "lane read", source: self.diagnose(source) }
            }
        })
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let result = match self.child.try_lock() {
            Ok(mut child) => self.request_termination(&mut child),
            Err(std::sync::TryLockError::Poisoned(error)) => self.request_termination(&mut error.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => Err(self.failure("lock", "cancellation could not acquire child lock")),
        };
        if let Err(error) = result { self.report(&error); }
    }
}

impl Drop for BridgedClient {
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}

#[cfg(test)]
#[derive(Default)]
struct ChildFaults {
    hold_termination: bool,
    fail_terminate: bool,
    fail_reap: bool,
}

#[cfg(test)]
#[path = "lane_child_tests.rs"]
pub(super) mod tests;
