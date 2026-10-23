//! Test observer (feature `test-support`, Unix): the way a test holds itself to seeing the end of a process it did not
//! start itself. The end is read from a retained identity (a pidfd on Linux), never from a pid number the OS may have
//! given to a stranger.

use std::time::Duration;

/// A pid is gone once a probe fails, polled for up to 3 s (where there is no pidfd to read).
#[cfg(not(target_os = "linux"))]
fn gone(pid: i32) -> bool {
    (0..150).any(|_| {
        std::thread::sleep(Duration::from_millis(20));
        // SAFETY: signal 0 only probes the pid.
        unsafe { libc::kill(pid, 0) != 0 }
    })
}

/// A process a test started that is a descendant of a child, so the test holds no handle to it. A leftover is
/// built from its pid, or from a file the stub wrote the pid to, read when the pid is first needed; on Linux
/// the process is opened then as a pidfd, and from then on its end is read from that identity and never from the
/// number, which the OS may give to a stranger once init has reaped the process. The test must see it gone
/// ([`gone`](Self::gone)), or say it is alive at the test's end by design ([`kept_alive`](Self::kept_alive)); a
/// leftover dropped with neither fails the test, so a test that forgot to check the process it started cannot pass.
/// A leftover only watches: the pid came from a file a process wrote or from a descendant, not from this test's own
/// spawn, so the test never signals it. What a failing run leaves alive ends with the fixture's own bound (each stub
/// ends itself) or with the container the suite runs in.
pub struct Leftover {
    pid: std::cell::Cell<Option<i32>>,
    file: Option<std::path::PathBuf>,
    state: std::cell::Cell<Seen>,
    #[cfg(target_os = "linux")]
    identity: std::cell::RefCell<Option<crate::watched::Watched>>,
    /// The process was already gone when its identity was opened (`ESRCH`).
    #[cfg(target_os = "linux")]
    gone_at_open: std::cell::Cell<bool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Seen {
    Unchecked,
    Gone,
    KeptAlive,
}

impl Leftover {
    pub fn of_pid(pid: i32) -> Self {
        let leftover = Self::empty(Some(pid), None);
        leftover.open_identity(pid);
        leftover
    }

    pub fn of_file(file: impl Into<std::path::PathBuf>) -> Self {
        Self::empty(None, Some(file.into()))
    }

    fn empty(pid: Option<i32>, file: Option<std::path::PathBuf>) -> Self {
        Leftover {
            pid: std::cell::Cell::new(pid),
            file,
            state: std::cell::Cell::new(Seen::Unchecked),
            #[cfg(target_os = "linux")]
            identity: std::cell::RefCell::new(None),
            #[cfg(target_os = "linux")]
            gone_at_open: std::cell::Cell::new(false),
        }
    }

    /// Open the process as an identity while it is alive. Only `ESRCH` and `EINVAL` mean it is already gone (seen gone); any other
    /// refusal is no observation, and the test fails rather than read it as the process's end.
    #[cfg(target_os = "linux")]
    fn open_identity(&self, pid: i32) {
        match crate::watched::Watched::try_open(pid) {
            Ok(identity) => *self.identity.borrow_mut() = Some(identity),
            // EINVAL: the pid number was reused by a thread of another process, and a live process is its own group leader.
            Err(e) if matches!(e.raw_os_error(), Some(libc::ESRCH | libc::EINVAL)) => self.gone_at_open.set(true),
            Err(e) => panic!("cannot observe pid {pid}: {e}"),
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn open_identity(&self, _pid: i32) {}

    pub fn pid(&self) -> Option<i32> {
        if let (None, Some(file)) = (self.pid.get(), &self.file) {
            let pid = std::fs::read_to_string(file)
                .ok()
                .and_then(|text| text.trim().parse().ok());
            self.pid.set(pid);
            if let Some(pid) = pid {
                self.open_identity(pid);
            }
        }
        self.pid.get()
    }

    /// Whether the process is gone, polled for up to 3 s. On Linux this reads the identity (a pidfd readable at
    /// the exit, reaped or not); a process whose pid was never readable, or that was gone before it could be
    /// opened (`ESRCH`), counts as gone only in the second case.
    pub fn gone(&self) -> bool {
        let gone = match self.pid() {
            None => false,
            #[cfg(target_os = "linux")]
            Some(_) => self.gone_at_open.get() || self.identity.borrow().as_ref().is_some_and(|identity| identity.dead()),
            #[cfg(not(target_os = "linux"))]
            Some(pid) => gone(pid),
        };
        if gone {
            self.state.set(Seen::Gone);
        }
        gone
    }

    /// This process is alive at the test's end by design: its stub ends itself.
    pub fn kept_alive(&self) {
        self.state.set(Seen::KeptAlive);
    }
}

impl Drop for Leftover {
    fn drop(&mut self) {
        if self.state.get() == Seen::Gone {
            return;
        }
        let pid = self.pid();
        if self.state.get() == Seen::Unchecked && pid.is_some() && !std::thread::panicking() {
            panic!("a Leftover (pid {}) was dropped without its end observed: the test never checked that the process it started is gone", pid.unwrap_or_default());
        }
    }
}